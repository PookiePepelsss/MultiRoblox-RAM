// RobloxNative.exe -- precompiled native helper for Vanta Manager. It replaced
// three PowerShell scripts (mutex.ps1, closehandles.ps1, audiovol.ps1).
//
// Why: every PowerShell invocation paid for powershell.exe startup AND an
// Add-Type C# compile on each call. Closing singleton handles runs before
// *every* launch and setting volume runs on every slider change, so that
// overhead was felt constantly. This is the same C# compiled once, ahead of
// time, and spawned directly -- no PowerShell, no per-call JIT/compile.
//
// Usage:
//   RobloxNative.exe daemon
//
// One resident process for the entire Vanta Manager session: it holds the
// singleton mutexes, runs anti-AFK and the PID watcher on background threads,
// and serves everything else as requests over stdin/stdout. It exits when
// stdin hits EOF -- i.e. the moment Vanta Manager itself is gone -- so the
// helper can never outlive its parent or pile up one-per-launch.
//
// The single-shot subcommands that predate daemon mode (mutex, closehandles,
// volume, antiafk, pids, watch, capture) are gone: nothing spawned them any
// more, and two of them were resident processes outside the singleton guard.
// For manual debugging, run "RobloxNative.exe daemon" in a console and type
// request lines such as "1|pids" by hand.
//
// Line protocol (UTF-8, '|' separated, one line per message, '\n' endings):
//   in   <id>|<cmd>[|arg...]
//   out  R|<id>|OK|<payload>
//        R|<id>|ERR|<message>
//        E|<event>|<payload>
// Requests are answered by id, so a slow capture never head-of-line blocks a
// pids poll. Events are pushed unsolicited:
//   E|READY|1 or 0          once at startup; 1 = ROBLOX_singletonMutex held
//   E|DUPLICATE|<text>      another helper owns the singleton; exiting
//   E|PIDS|<pid,pid,...>    PID watcher tick
//   E|AFK|<pid>             anti-AFK tapped that instance
//   E|AFKFAIL|<pid>         anti-AFK keeps failing to reach that instance
// Requests:
//   ping                          -> pong
//   pids                          -> comma-separated RobloxPlayerBeta PIDs
//   closehandles[|pid,pid,...]    -> number of singleton handles closed; the
//                                    listed PIDs are exempt
//   mutex|on, mutex|off, mutex|rehold -> 1 if held afterwards, else 0
//   volume|<0-100>                -> number of audio sessions changed
//   watch|<ms>                    -> ok (0 stops the watcher, minimum 250)
//   antiafk|<sec>[|<vk>]          -> effective deadline, or off when sec <= 0
//   capture|<pid>[|x|y|w|h]       -> base64 PNG; x/y/w/h are 0-1 fractions
//                                    with '.' decimals
//   preview|<pid>|<maxW>|<quality> -> base64 JPEG of the game picture, at most
//                                    maxW wide; never focuses or restores the
//                                    window. MIN (minimized) or BLANK (the window
//                                    can't draw itself) instead of a picture
//   tile|<pid,pid,...>|<w>|<h>[|<corner>|<gap>[|<append>]]
//                                 -> windows laid out as w x h client areas in rows,
//                                    from corner 0 top left, 1 top right, 2 bottom
//                                    left, 3 bottom right, gap pixels apart (0 =
//                                    touching); how many were placed. With append 1
//                                    the windows already tiled stay as they are and
//                                    only new ones take the free cells
//   untile                        -> windows put back as they were; how many
//   signin|on, signin|off         -> 1 if the shared sign-in file is locked afterwards, else 0
//   shutdown                      -> ok, then the process exits
// "0|shutdown" is a no-reply fast path handled on the read loop itself.
//
// Build with the .NET Framework compiler that ships with Windows (C# 5):
//   csc /nologo /optimize+ /platform:x64 /target:exe /r:System.Drawing.dll /out:RobloxNative.exe RobloxNative.cs

using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.Drawing.Imaging;
using System.Globalization;
using System.IO;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

// Identity for the compiled binary. csc emits these into the PE version
// resource, so the file shows a company, product and version in its
// properties instead of being a nameless 0.0.0.0 executable.
//
// This is not cosmetic. This helper legitimately has to enumerate system
// handles, open other processes and synthesise keystrokes, which on its own
// reads like a hacktool to heuristic scanners; an unsigned binary carrying no
// identity whatsoever removes the one cheap signal that it belongs to a real
// product. Code signing would be the real fix and is deliberately not done,
// so this metadata is one of the few signals left; shipping something
// anonymous makes a false positive far more likely.
[assembly: AssemblyTitle("Vanta Manager Native Helper")]
[assembly: AssemblyDescription("Multi-instance, window and audio helper for Vanta Manager")]
[assembly: AssemblyCompany("PookiePepelsss")]
[assembly: AssemblyProduct("Vanta Manager")]
[assembly: AssemblyCopyright("Copyright (c) 2026 PookiePepelsss")]
[assembly: AssemblyVersion("1.2.6.0")]
[assembly: AssemblyFileVersion("1.2.6.0")]

// ── Entry point ─────────────────────────────────────────────────────────────
internal static class RobloxNative
{
    private static int Main(string[] args)
    {
        // An exception escaping any thread other than a thread-pool work item
        // terminates the process. Every worker catches its own, but if one
        // ever slips through, at least leave a reason on stderr (which the
        // app forwards to its log) instead of vanishing silently.
        AppDomain.CurrentDomain.UnhandledException += delegate(object sender, UnhandledExceptionEventArgs e)
        {
            Protocol.Warn("RobloxNative unhandled: " + e.ExceptionObject);
        };

        string cmd = args.Length > 0 ? args[0].ToLowerInvariant() : "";
        if (cmd != "daemon")
        {
            Console.Error.WriteLine("Usage: RobloxNative.exe daemon   (then send <id>|<cmd>[|arg...] lines on stdin)");
            return 2;
        }

        try
        {
            return Daemon.Run();
        }
        catch (Exception ex)
        {
            Protocol.Warn("RobloxNative fatal: " + ex);
            return 1;
        }
    }
}

// ── Protocol: stdout writer and argument parsing ────────────────────────────
// Every stdout line goes through WriteLine under one lock. Replies come from
// thread-pool workers and events from the watcher/anti-AFK threads, so
// without the lock two lines could interleave mid-line and desync the
// reader in helper.rs.
internal static class Protocol
{
    static readonly object _outLock = new object();
    static StreamWriter _out;

    public static void Open()
    {
        lock (_outLock)
        {
            _out = new StreamWriter(Console.OpenStandardOutput(), new UTF8Encoding(false), 1 << 16);
            _out.AutoFlush = false;
        }
    }

    public static void Reply(string id, string payload) { WriteLine("R|", id, "|OK|", payload); }
    public static void Fail(string id, string message)  { WriteLine("R|", id, "|ERR|", Sanitize(message)); }
    public static void Emit(string name, string payload) { WriteLine("E|", name, "|", payload); }

    // stderr is a separate pipe the app only logs, so it can't desync the
    // protocol; Console.Error is already synchronized.
    public static void Warn(string message)
    {
        try { Console.Error.WriteLine(message); } catch { }
    }

    // Written in pieces rather than concatenated first: a capture payload is
    // a multi-megabyte base64 string, and concatenating would copy it again.
    static void WriteLine(string a, string b, string c, string d)
    {
        lock (_outLock)
        {
            if (_out == null) return;
            try
            {
                _out.Write(a); _out.Write(b); _out.Write(c); _out.Write(d ?? "");
                _out.Write('\n');
                _out.Flush();
            }
            catch { } // pipe gone: stdin EOF follows and shuts us down
        }
    }

    // Error text is the only field that can contain a separator or newline;
    // neutralise both so one bad message can't desync the whole stream.
    static string Sanitize(string s)
    {
        if (s == null) return "";
        return s.Replace("\r", " ").Replace("\n", " ").Replace("|", "/");
    }

    public static string Arg(string[] parts, int index)
    {
        return index < parts.Length ? parts[index] : null;
    }

    // Always invariant culture: the Rust side formats numbers with '.', which
    // current-culture parsing would reject on a comma-decimal locale --
    // silently turning every cropped capture into a full-window one.
    public static bool TryParseInt(string s, out int value)
    {
        value = 0;
        if (s == null) return false;
        return int.TryParse(s.Trim(), NumberStyles.Integer, CultureInfo.InvariantCulture, out value);
    }

    public static bool TryParseDouble(string s, out double value)
    {
        value = 0;
        if (s == null) return false;
        return double.TryParse(s.Trim(), NumberStyles.Float, CultureInfo.InvariantCulture, out value)
            && !double.IsNaN(value) && !double.IsInfinity(value);
    }
}

// ── Daemon: the one and only helper process ─────────────────────────────────
internal static class Daemon
{
    // Named per user session (no "Global\") -- Vanta Manager is a per-user app.
    // Bump the suffix if the line protocol ever changes incompatibly; the
    // name must stay in sync with another_helper_running() in helper.rs.
    const string SingletonName = "MultiRoblox_NativeHelper_v1";
    static Mutex _singleInstance; // rooted so GC can't finalize it and drop the claim

    static readonly Worker _pidWatcher = new Worker("PidWatch", 2000);
    // Bounded: a tap in progress holds the loop for a few hundred ms.
    static readonly Worker _antiAfk = new Worker("AntiAfk", 3000);
    // Keeps tiled windows at their small size; runs only while some are tiled.
    static readonly Worker _layoutKeep = new Worker("LayoutKeep", 2000);

    public static int Run()
    {
        Protocol.Open();

        // Belt to the caller's braces: the app already sweeps strays and
        // serializes its own spawns, but this makes "exactly one helper" an
        // OS-enforced invariant instead of a promise the caller has to keep.
        // Checked before touching Roblox's mutexes so a stray second copy
        // can't disturb the incumbent on its way out.
        bool createdNew;
        _singleInstance = new Mutex(true, SingletonName, out createdNew);
        if (!createdNew)
        {
            Protocol.Emit("DUPLICATE", "another helper is already running");
            return 3;
        }

        // Before any window call: capture needs real pixel coordinates.
        WindowFocus.EnableDpiAwareness();

        MutexHolder.Start();
        Protocol.Emit("READY", MutexHolder.Held ? "1" : "0");

        try
        {
            using (var stdin = new StreamReader(Console.OpenStandardInput(), Encoding.UTF8))
            {
                string line;
                while ((line = stdin.ReadLine()) != null)
                {
                    string request = line.Trim();
                    if (request.Length == 0) continue;
                    if (request == "0|shutdown") break; // fast path, no reply expected
                    // Off the read loop: a capture takes ~200ms and closehandles
                    // can take seconds, and neither may stall a pids poll behind
                    // it. Responses carry their request id, so out-of-order is fine.
                    ThreadPool.QueueUserWorkItem(Dispatch, request);
                }
            }
        }
        finally
        {
            // stdin EOF means Vanta Manager is gone -- exited cleanly, crashed,
            // or was force-killed. Either way this process must not survive
            // it, so there is never an orphaned helper holding Roblox's mutex.
            // Stopping the workers first lets anti-AFK put the system
            // foreground-lock timeout back; returning from Main would kill
            // its background thread before its cleanup runs.
            StopWorkers();
            // A window this helper hid would otherwise stay invisible, with no
            // taskbar button, once nothing is left that can show it again.
            HiddenWindows.ShowAll();
        }
        return 0;
    }

    static void StopWorkers()
    {
        _pidWatcher.Stop();
        _antiAfk.Stop();
        _layoutKeep.Stop();
    }

    static void Dispatch(object state)
    {
        string line = (string)state;
        string[] p = line.Split('|');
        string id = p[0];
        string cmd = p.Length > 1 ? p[1].Trim().ToLowerInvariant() : "";
        try
        {
            switch (cmd)
            {
                case "ping":         Protocol.Reply(id, "pong"); break;
                case "pids":         Protocol.Reply(id, RobloxProcesses.PidList()); break;
                case "closehandles": HandleCloseHandles(id, p); break;
                case "mutex":        HandleMutex(id, p); break;
                case "volume":       HandleVolume(id, p); break;
                case "watch":        HandleWatch(id, p); break;
                case "antiafk":      HandleAntiAfk(id, p); break;
                case "capture":      HandleCapture(id, p); break;
                case "preview":      HandlePreview(id, p); break;
                case "tile":         HandleTile(id, p); break;
                case "winmode":      HandleWinMode(id, p); break;
                case "untile":
                    _layoutKeep.Stop();
                    Protocol.Reply(id, WindowLayout.Untile().ToString(CultureInfo.InvariantCulture));
                    break;
                case "signin":       Protocol.Reply(id, SignInLock.Apply(Protocol.Arg(p, 2) != "off") ? "1" : "0"); break;
                case "shutdown":
                    Protocol.Reply(id, "ok");
                    // Stop the workers first: Environment.Exit kills background
                    // threads outright, so anti-AFK's cleanup -- which restores
                    // the system foreground-lock timeout -- would never run.
                    StopWorkers();
                    HiddenWindows.ShowAll();
                    Environment.Exit(0);
                    break;
                default:
                    Protocol.Fail(id, "unknown command: " + cmd);
                    break;
            }
        }
        catch (Exception ex)
        {
            Protocol.Fail(id, ex.Message);
        }
    }

    static void HandleCloseHandles(string id, string[] p)
    {
        HashSet<int> exempt = null;
        string list = Protocol.Arg(p, 2);
        if (!string.IsNullOrWhiteSpace(list))
        {
            exempt = new HashSet<int>();
            foreach (string token in list.Split(','))
            {
                int pid;
                if (Protocol.TryParseInt(token, out pid) && pid > 0) exempt.Add(pid);
            }
        }
        Protocol.Reply(id, HandleCloser.CloseRobloxSingletonHandles(exempt).ToString(CultureInfo.InvariantCulture));
    }

    static void HandleMutex(string id, string[] p)
    {
        string mode = (Protocol.Arg(p, 2) ?? "on").Trim().ToLowerInvariant();
        bool held;
        if (mode == "off")         held = MutexHolder.Apply(false, false, 15000);
        else if (mode == "rehold") held = MutexHolder.Apply(true, true, 15000);
        else                       held = MutexHolder.Apply(true, false, 15000);
        Protocol.Reply(id, held ? "1" : "0");
    }

    static void HandleVolume(string id, string[] p)
    {
        int pct;
        if (!Protocol.TryParseInt(Protocol.Arg(p, 2), out pct))
        {
            double d;
            pct = Protocol.TryParseDouble(Protocol.Arg(p, 2), out d) ? (int)Math.Round(d) : 0;
        }
        if (pct < 0) pct = 0;
        if (pct > 100) pct = 100;

        int[] pids = RobloxProcesses.Snapshot();
        if (pids == null) throw new InvalidOperationException("could not enumerate Roblox processes");
        int changed = pids.Length == 0 ? 0 : AudioVolume.Apply(pct / 100.0f, new HashSet<int>(pids));
        Protocol.Reply(id, changed.ToString(CultureInfo.InvariantCulture));
    }

    static void HandleWatch(string id, string[] p)
    {
        int ms;
        if (!Protocol.TryParseInt(Protocol.Arg(p, 2), out ms)) ms = 0;
        if (ms <= 0)
        {
            _pidWatcher.Stop();
        }
        else
        {
            int interval = ms < 250 ? 250 : ms;
            _pidWatcher.Start(delegate(WaitHandle stop) { RunPidWatch(stop, interval); });
        }
        Protocol.Reply(id, "ok");
    }

    [DllImport("kernel32.dll", SetLastError = true)] static extern IntPtr OpenProcess(int access, bool inherit, int pid);
    const int SYNCHRONIZE = 0x00100000;
    // WaitAny takes at most 64 handles, one of which is the stop signal.
    const int MaxWatchedProcesses = 63;

    // Waits on the Roblox processes themselves instead of scanning on a short
    // timer: Windows wakes this thread the moment one exits, so a close is
    // seen at once, and between exits the process list is only re-read every
    // `rescanMs` to pick up newly started clients. The list is only sent when
    // it changed. A failed scan is not "zero Roblox processes": reporting it
    // as an empty list would make the app treat every instance as closed, so
    // that pass is skipped.
    static void RunPidWatch(WaitHandle stop, int rescanMs)
    {
        var pids = new List<int>();
        var text = new StringBuilder(64);
        var waits = new Dictionary<int, ProcessWaitHandle>();
        // Exited but possibly still listed for a moment: never reported alive
        // again, and never waited on again (it would wake this loop at once).
        var exited = new HashSet<int>();
        string last = null;
        try
        {
            while (true)
            {
                if (RobloxProcesses.Find(pids))
                {
                    exited.IntersectWith(pids);
                    pids.RemoveAll(exited.Contains);
                    pids.Sort();
                    string now = RobloxProcesses.Join(pids, text);
                    if (now != last) { Protocol.Emit("PIDS", now); last = now; }

                    var gone = new List<int>();
                    foreach (int known in waits.Keys) if (!pids.Contains(known)) gone.Add(known);
                    foreach (int g in gone) { waits[g].Dispose(); waits.Remove(g); }
                    foreach (int pid in pids)
                    {
                        if (waits.Count >= MaxWatchedProcesses) break;
                        if (waits.ContainsKey(pid)) continue;
                        IntPtr h = OpenProcess(SYNCHRONIZE, false, pid);
                        if (h != IntPtr.Zero) waits[pid] = new ProcessWaitHandle(h);
                    }
                }

                var set = new WaitHandle[waits.Count + 1];
                var order = new int[waits.Count + 1];
                set[0] = stop;
                int k = 1;
                foreach (KeyValuePair<int, ProcessWaitHandle> w in waits) { set[k] = w.Value; order[k] = w.Key; k++; }
                int hit = WaitHandle.WaitAny(set, rescanMs);
                if (hit == 0) return;
                if (hit > 0 && hit < set.Length)
                {
                    int pid = order[hit];
                    exited.Add(pid);
                    waits[pid].Dispose();
                    waits.Remove(pid);
                }
            }
        }
        finally
        {
            foreach (ProcessWaitHandle w in waits.Values) w.Dispose();
        }
    }

    sealed class ProcessWaitHandle : WaitHandle
    {
        public ProcessWaitHandle(IntPtr handle)
        {
            SafeWaitHandle = new Microsoft.Win32.SafeHandles.SafeWaitHandle(handle, true);
        }
    }

    // winmode|pid|min           -> minimized without taking focus
    // winmode|pid|small|w|h     -> game area resized to w x h, where it is
    // winmode|pid|hide          -> minimized, then hidden (no window, no taskbar button)
    // winmode|pid|show          -> a hidden window back on screen, in front
    // winmode|pid|unhide        -> a hidden window back on the taskbar, minimized
    // "0" when the window isn't there yet (the caller tries again), else "1".
    static void HandleWinMode(string id, string[] p)
    {
        int pid;
        if (!Protocol.TryParseInt(Protocol.Arg(p, 2), out pid) || pid <= 0)
        {
            Protocol.Fail(id, "Invalid PID");
            return;
        }
        string mode = (Protocol.Arg(p, 3) ?? "").Trim().ToLowerInvariant();
        if (mode == "hide") { Protocol.Reply(id, HiddenWindows.Hide(pid) ? "1" : "0"); return; }
        if (mode == "show") { Protocol.Reply(id, HiddenWindows.Show(pid) ? "1" : "0"); return; }
        if (mode == "unhide") { Protocol.Reply(id, HiddenWindows.Unhide(pid) ? "1" : "0"); return; }
        IntPtr hwnd = WindowFocus.LargestWindowOf(pid);
        if (hwnd == IntPtr.Zero)
        {
            Protocol.Reply(id, "0");
            return;
        }
        if (mode == "min")
        {
            WindowFocus.ShowWindow(hwnd, 7); // SW_SHOWMINNOACTIVE
            Protocol.Reply(id, "1");
            return;
        }
        if (mode == "small")
        {
            int w, h;
            if (!Protocol.TryParseInt(Protocol.Arg(p, 4), out w)) w = 640;
            if (!Protocol.TryParseInt(Protocol.Arg(p, 5), out h)) h = 360;
            Protocol.Reply(id, WindowLayout.ShrinkOne(hwnd, w, h) ? "1" : "0");
            return;
        }
        Protocol.Fail(id, "unknown window mode: " + mode);
    }

    static void HandleAntiAfk(string id, string[] p)
    {
        int sec;
        if (!Protocol.TryParseInt(Protocol.Arg(p, 2), out sec)) sec = 0;
        int vk = AntiAfk.DefaultVirtualKey;
        int v;
        if (Protocol.TryParseInt(Protocol.Arg(p, 3), out v) && v > 0 && v < 256) vk = v;

        if (sec <= 0)
        {
            _antiAfk.Stop();
            Protocol.Reply(id, "off");
            return;
        }
        if (sec < 60) sec = 60;
        if (sec > 1020) sec = 1020; // leaves minutes of retries before the ~20-min kick

        int deadline = sec;
        _antiAfk.Start(delegate(WaitHandle stop)
        {
            AntiAfk.RunLoop(deadline, (byte)vk, delegate(uint pid)
            {
                Protocol.Emit("AFK", pid.ToString(CultureInfo.InvariantCulture));
            }, delegate(uint pid)
            {
                Protocol.Emit("AFKFAIL", pid.ToString(CultureInfo.InvariantCulture));
            }, stop);
        });
        Protocol.Reply(id, sec.ToString(CultureInfo.InvariantCulture));
    }

    static void HandlePreview(string id, string[] p)
    {
        int pid, maxW, quality;
        if (!Protocol.TryParseInt(Protocol.Arg(p, 2), out pid) || pid <= 0)
        {
            Protocol.Fail(id, "Invalid PID");
            return;
        }
        if (!Protocol.TryParseInt(Protocol.Arg(p, 3), out maxW)) maxW = 160;
        if (!Protocol.TryParseInt(Protocol.Arg(p, 4), out quality)) quality = 40;
        maxW = Math.Max(16, Math.Min(maxW, 640));
        quality = Math.Max(5, Math.Min(quality, 90));
        Protocol.Reply(id, WindowPreview.Jpeg(pid, maxW, quality));
    }

    static void HandleTile(string id, string[] p)
    {
        var pids = new List<int>();
        string list = Protocol.Arg(p, 2);
        if (!string.IsNullOrWhiteSpace(list))
        {
            foreach (string token in list.Split(','))
            {
                int pid;
                if (Protocol.TryParseInt(token, out pid) && pid > 0) pids.Add(pid);
            }
        }
        int w, h;
        if (!Protocol.TryParseInt(Protocol.Arg(p, 3), out w)) w = 320;
        if (!Protocol.TryParseInt(Protocol.Arg(p, 4), out h)) h = 180;
        int corner, gap, append;
        if (!Protocol.TryParseInt(Protocol.Arg(p, 5), out corner)) corner = 0;
        if (!Protocol.TryParseInt(Protocol.Arg(p, 6), out gap)) gap = 0;
        if (!Protocol.TryParseInt(Protocol.Arg(p, 7), out append)) append = 0;
        w = Math.Max(160, Math.Min(w, 1280));
        h = Math.Max(90, Math.Min(h, 720));
        corner = Math.Max(0, Math.Min(corner, 3));
        gap = Math.Max(0, Math.Min(gap, 64));
        int placed = WindowLayout.Tile(pids, w, h, corner, gap, append == 1);
        if (placed > 0) _layoutKeep.Start(WindowLayout.KeepLoop);
        Protocol.Reply(id, placed.ToString(CultureInfo.InvariantCulture));
    }

    static void HandleCapture(string id, string[] p)
    {
        int pid;
        if (!Protocol.TryParseInt(Protocol.Arg(p, 2), out pid) || pid <= 0)
        {
            Protocol.Fail(id, "Invalid PID");
            return;
        }
        double x = 0, y = 0, w = 1, h = 1;
        bool crop = p.Length >= 7
            && Protocol.TryParseDouble(p[3], out x) && Protocol.TryParseDouble(p[4], out y)
            && Protocol.TryParseDouble(p[5], out w) && Protocol.TryParseDouble(p[6], out h);
        if (!crop) { x = 0; y = 0; w = 1; h = 1; }
        Protocol.Reply(id, WindowCapture.CaptureBase64(pid, crop, x, y, w, h));
    }
}

// ── Worker: one restartable background thread with a stop signal ───────────
// Used for the PID watcher and anti-AFK. There is only ever one helper
// process, so these loops have to be stoppable and reconfigurable in place
// rather than killed.
internal sealed class Worker
{
    readonly string _name;
    readonly int _joinTimeoutMs;
    readonly object _lock = new object();
    Thread _thread;
    ManualResetEvent _stop;

    public Worker(string name, int joinTimeoutMs)
    {
        _name = name;
        _joinTimeoutMs = joinTimeoutMs;
    }

    public void Start(Action<WaitHandle> body)
    {
        lock (_lock)
        {
            StopLocked();
            var stop = new ManualResetEvent(false);
            string name = _name;
            var thread = new Thread(delegate()
            {
                // An exception escaping a plain thread kills the whole process.
                try { body(stop); }
                catch (Exception ex) { Protocol.Warn(name + ": " + ex); }
            });
            thread.IsBackground = true;
            thread.Name = name;
            thread.Start();
            _thread = thread;
            _stop = stop;
        }
    }

    public void Stop()
    {
        lock (_lock) { StopLocked(); }
    }

    void StopLocked()
    {
        if (_thread == null) return;
        _stop.Set();
        if (_thread.Join(_joinTimeoutMs))
            _stop.Dispose(); // only once nothing can wait on it any more
        else
            Protocol.Warn(_name + ": worker did not stop within " + _joinTimeoutMs + "ms");
        _thread = null;
        _stop = null;
    }
}

// ── NtQuerySystemInformation with a reusable, growable buffer ───────────────
// The process list and the system handle table are both read this way, each
// owner serializing access itself. The process-list buffer is kept for the
// life of the process, so a watcher tick allocates nothing native; the much
// larger handle-table buffer is trimmed after each scan but remembers its
// size, so a pre-launch scan makes one kernel query instead of growing
// through a chain of failed ones.
internal sealed class SystemInfoBuffer
{
    [DllImport("ntdll.dll")]
    static extern int NtQuerySystemInformation(int infoClass, IntPtr buffer, int length, out int returnLength);

    const int STATUS_INFO_LENGTH_MISMATCH = unchecked((int)0xC0000004);
    const int STATUS_BUFFER_TOO_SMALL = unchecked((int)0xC0000023);
    const int MaxSize = 512 << 20;

    readonly int _infoClass;
    IntPtr _pointer;
    int _size;

    public SystemInfoBuffer(int infoClass, int initialSize)
    {
        _infoClass = infoClass;
        _size = initialSize;
    }

    public IntPtr Pointer { get { return _pointer; } }
    public int Size { get { return _size; } }

    public bool Query()
    {
        if (_pointer == IntPtr.Zero) Allocate(_size);
        for (int attempt = 0; attempt < 8; attempt++)
        {
            int needed;
            int status = NtQuerySystemInformation(_infoClass, _pointer, _size, out needed);
            if (status == 0) return true;
            if (status != STATUS_INFO_LENGTH_MISMATCH && status != STATUS_BUFFER_TOO_SMALL) return false;
            // Headroom on top of what was asked for: the table keeps growing
            // between the two calls on a busy machine.
            long next = needed > _size ? (long)needed + needed / 4 : (long)_size * 2;
            if (next > MaxSize) return false;
            Allocate((int)next);
        }
        return false;
    }

    // Frees the memory but keeps the learned size, so the next Query still
    // allocates once at the right size instead of growing through several
    // failed calls (each of which walks the whole table in the kernel).
    public void Trim()
    {
        if (_pointer == IntPtr.Zero) return;
        Marshal.FreeHGlobal(_pointer);
        _pointer = IntPtr.Zero;
    }

    void Allocate(int size)
    {
        if (_pointer != IntPtr.Zero)
        {
            Marshal.FreeHGlobal(_pointer);
            _pointer = IntPtr.Zero;
        }
        _pointer = Marshal.AllocHGlobal(size);
        _size = size;
    }
}

// ── Roblox process enumeration ──────────────────────────────────────────────
// Reads SYSTEM_PROCESS_INFORMATION straight from a reused buffer and compares
// image names in place. Process.GetProcessesByName builds a Process object,
// a ProcessInfo and a ThreadInfo per thread for EVERY process on the machine
// on each call (and leaves the Process objects for the finalizer); at the
// watcher's 2s cadence that was the bulk of the helper's idle allocation.
internal static class RobloxProcesses
{
    const int SystemProcessInformation = 5;
    const string ImageName = "robloxplayerbeta.exe"; // lower case; compared case-insensitively

    // SYSTEM_PROCESS_INFORMATION field offsets.
    static readonly int ImageNameLengthOffset = 0x38;
    static readonly int ImageNameBufferOffset = IntPtr.Size == 8 ? 0x40 : 0x3C;
    static readonly int ProcessIdOffset = IntPtr.Size == 8 ? 0x50 : 0x44;

    static readonly object _lock = new object();
    static readonly SystemInfoBuffer _buffer = new SystemInfoBuffer(SystemProcessInformation, 512 * 1024);

    // Fills `into` with every RobloxPlayerBeta PID. False means the scan
    // itself failed, which callers must not confuse with "none running".
    public static bool Find(List<int> into)
    {
        into.Clear();
        lock (_lock)
        {
            if (!_buffer.Query()) return false;
            long start = _buffer.Pointer.ToInt64();
            long end = start + _buffer.Size;
            long offset = 0;
            while (true)
            {
                IntPtr entry = new IntPtr(start + offset);
                if (ImageNameMatches(entry))
                    into.Add((int)Marshal.ReadIntPtr(entry, ProcessIdOffset).ToInt64());
                int next = Marshal.ReadInt32(entry);
                if (next <= 0) break;
                offset += next;
                if (start + offset >= end) break;
            }
        }
        return true;
    }

    // Null when the scan failed.
    public static int[] Snapshot()
    {
        var pids = new List<int>();
        return Find(pids) ? pids.ToArray() : null;
    }

    public static string PidList()
    {
        var pids = new List<int>();
        if (!Find(pids)) throw new InvalidOperationException("could not enumerate Roblox processes");
        return Join(pids, new StringBuilder(64));
    }

    public static string Join(List<int> pids, StringBuilder text)
    {
        text.Length = 0;
        for (int i = 0; i < pids.Count; i++)
        {
            if (i > 0) text.Append(',');
            text.Append(pids[i].ToString(CultureInfo.InvariantCulture));
        }
        return text.ToString();
    }

    static bool ImageNameMatches(IntPtr entry)
    {
        int bytes = (ushort)Marshal.ReadInt16(entry, ImageNameLengthOffset);
        if (bytes != ImageName.Length * 2) return false;
        IntPtr chars = Marshal.ReadIntPtr(entry, ImageNameBufferOffset);
        if (chars == IntPtr.Zero) return false;
        for (int i = 0; i < ImageName.Length; i++)
        {
            char c = (char)Marshal.ReadInt16(chars, i * 2);
            if (c >= 'A' && c <= 'Z') c = (char)(c + 32);
            if (c != ImageName[i]) return false;
        }
        return true;
    }
}

// ── Singleton mutex ownership ───────────────────────────────────────────────
// A Windows mutex is owned by the THREAD that acquired it -- if that thread
// exits, the mutex is abandoned and Roblox's singleton check starts passing
// again. So exactly one dedicated thread acquires both names and then parks
// forever, servicing hold/release/re-hold requests without ever unwinding.
// (The old design got this for free by dedicating a whole PROCESS to it; the
// point of daemon mode is that we no longer spend a process per concern.)
internal static class MutexHolder
{
    const string SingletonMutexName = "ROBLOX_singletonMutex";
    const string SingletonEventName = "ROBLOX_singletonEvent";

    // Only ever touched by the holder thread.
    static Mutex _singleton, _event;

    static readonly ManualResetEvent _ready = new ManualResetEvent(false);
    static readonly object _applyLock = new object();
    static readonly object _sync = new object();

    // Guarded by _sync. Requests are numbered so a caller waits for ITS
    // request to be applied, not for whatever the holder happened to finish
    // next (e.g. the startup scan, which used to complete a toggle early).
    static bool _wantHeld = true;
    static bool _reholdRequested;
    static long _requested, _applied;

    static volatile bool _held;

    public static bool Held { get { return _held; } }

    public static void Start()
    {
        var t = new Thread(Loop);
        t.IsBackground = true;
        t.Name = "MutexHolder";
        t.Start();
        // Matches the old "MUTEX_HELD within 8s" contract the launch path
        // relied on, so a launch can never race an unheld mutex.
        _ready.WaitOne(8000);
    }

    // Serialized so two racing settings toggles can't interleave a release
    // with a re-acquire. Blocks until the holder thread has actually applied
    // the change -- callers (kill-all, multi-instance toggle) need the new
    // state to be real before they return, not merely queued.
    public static bool Apply(bool hold, bool rehold, int timeoutMs)
    {
        lock (_applyLock)
        {
            lock (_sync)
            {
                _wantHeld = hold;
                if (rehold) _reholdRequested = true;
                long ticket = ++_requested;
                Monitor.PulseAll(_sync);

                int deadline = Environment.TickCount + timeoutMs;
                while (_applied < ticket)
                {
                    int remaining = deadline - Environment.TickCount;
                    if (remaining <= 0) break;
                    Monitor.Wait(_sync, remaining);
                }
            }
            return _held;
        }
    }

    static void Loop()
    {
        Acquire();
        _ready.Set();
        SlowPart();

        while (true)
        {
            long target;
            bool hold, rehold;
            lock (_sync)
            {
                while (_applied == _requested) Monitor.Wait(_sync);
                target = _requested;
                hold = _wantHeld;
                rehold = _reholdRequested;
                _reholdRequested = false;
            }

            // Never let an exception unwind this thread: its exit would
            // abandon the mutex it exists to hold.
            try
            {
                if (!hold) Release();
                else if (!_held || rehold)
                {
                    Release(); // drop any stale/abandoned ownership first
                    Acquire();
                    SlowPart();
                }
                // The mutex is fine but the event name was lost to a client
                // that started during the startup scan. Its handle would make
                // every later client quit, so take the name back here; this
                // runs before each launch, which is what makes it self-healing.
                else if (_event == null) SlowPart();
            }
            catch (Exception ex) { Protocol.Warn("MutexHolder: " + ex.Message); }

            lock (_sync)
            {
                _applied = target;
                Monitor.PulseAll(_sync);
            }
        }
    }

    // Hold ROBLOX_singletonMutex first: cheap, and the object Roblox's
    // singleton check keys off.
    //
    // A client that was already open (started normally, or while nothing held
    // the name) owns this mutex, which used to leave us at READY|0 for good. A
    // mutex can't be taken from its owner, but closing that client's handle
    // removes the name so we can create our own. Only done when contested, so
    // the normal path never pays for a handle-table scan here.
    //
    // Note a separate client check, logged as "NoReload Detector": a client
    // started with NO arguments hands off to a running one and exits within
    // milliseconds whatever we hold. Launches here always pass a URI, so it
    // doesn't apply to them, but it makes bare-exe multi-instance tests misleading.
    static void Acquire()
    {
        _singleton = TryHold(SingletonMutexName, "HoldMutex");
        if (_singleton == null)
        {
            try { HandleCloser.CloseRobloxSingletonHandles(null, true); }
            catch (Exception ex) { Protocol.Warn("CloseHandles(contested): " + ex.Message); }
            _singleton = TryHold(SingletonMutexName, "HoldMutex");
        }
        _held = _singleton != null;
    }

    // The slow half: close any singleton-event handles Roblox already opened,
    // then hold that name too. Runs after readiness is signalled so a cold
    // start never blocks the first launch on a full handle-table scan.
    static void SlowPart()
    {
        try { HandleCloser.CloseRobloxSingletonHandles(null); }
        catch (Exception ex) { Protocol.Warn("CloseHandles(hold): " + ex.Message); }
        _event = TryHold(SingletonEventName, "HoldEventMutex");
    }

    // Returns the mutex only if this thread now owns it. A mutex we opened
    // but could not own is closed again rather than kept: holding a handle
    // without ownership does nothing for us, and Release() would only throw
    // trying to release it.
    static Mutex TryHold(string name, string what)
    {
        Mutex m = null;
        try
        {
            bool created;
            m = new Mutex(true, name, out created);
            bool owned = created;
            if (!created)
            {
                try { owned = m.WaitOne(0); }
                catch (AbandonedMutexException) { owned = true; } // ownership passes to us
            }
            if (owned) return m;
            m.Dispose();
            return null;
        }
        catch (Exception ex)
        {
            if (m != null) { try { m.Dispose(); } catch { } }
            Protocol.Warn(what + ": " + ex.Message);
            return null;
        }
    }

    static void Release()
    {
        ReleaseOne(ref _event);
        ReleaseOne(ref _singleton);
        _held = false;
    }

    static void ReleaseOne(ref Mutex m)
    {
        if (m == null) return;
        try { m.ReleaseMutex(); } catch { }
        try { m.Dispose(); } catch { }
        m = null;
    }
}

// ── Shared sign-in file ─────────────────────────────────────────────────────
// Every Roblox client reads and writes the one RobloxCookies.dat, so with
// several accounts open each client can pick up another's sign-in, for example
// when it reconnects or teleports (error 773). Holding the file open with no
// sharing keeps every client on the session its launch ticket gave it.
//
// Opened read-only and never written, so what's in the file is left exactly as
// it was. Closed when the last launched instance is gone, and by Windows if this
// process dies, so a crash can't leave it locked.
internal static class SignInLock
{
    const int Attempts = 6;
    const int RetryMs = 150;

    static readonly object _gate = new object();
    static FileStream _file;

    public static bool Apply(bool hold)
    {
        lock (_gate)
        {
            if (!hold)
            {
                if (_file != null) { _file.Dispose(); _file = null; }
                return false;
            }
            if (_file != null) return true;
            string path = FilePath();
            if (path == null) return false;
            // A client only has the file open for a moment at a time.
            for (int attempt = 0; attempt < Attempts; attempt++)
            {
                try
                {
                    Directory.CreateDirectory(Path.GetDirectoryName(path));
                    _file = new FileStream(path, FileMode.OpenOrCreate, FileAccess.Read, FileShare.None);
                    return true;
                }
                catch (IOException) { }
                catch (UnauthorizedAccessException) { }
                Thread.Sleep(RetryMs);
            }
            return false;
        }
    }

    static string FilePath()
    {
        string local = Environment.GetEnvironmentVariable("LOCALAPPDATA");
        if (string.IsNullOrEmpty(local)) return null;
        return Path.Combine(local, "Roblox", "LocalStorage", "RobloxCookies.dat");
    }
}

// ── ROBLOX_singletonEvent handle closing (ported from closehandles.ps1) ─────
// Walks the system handle table and closes, inside each Roblox process, any
// handle whose object is named ROBLOX_singletonEvent. This is the hot path
// before every launch, so the scan is filtered cheaply before any syscall:
//   1. by PID, against the Roblox processes we were asked to touch;
//   2. by object type index -- only Event/Mutant/Semaphore objects can carry
//      the name. Each type index is resolved to its name once (by
//      duplicating the first handle seen of that type) and cached; type
//      indices are fixed until reboot, and this process lives one session.
// Each Roblox process is opened once per scan instead of once per handle,
// the query scratch buffer is reused, and the handle-table buffer starts at
// the size the last scan needed.
//
// The type filter also fixes a hang: querying the NAME of some file and
// named-pipe handles blocks forever, which could wedge a scan for good.
// Type queries never block, and names are only queried for sync objects.
internal static class HandleCloser
{
    [DllImport("kernel32.dll", SetLastError = true)] static extern IntPtr OpenProcess(int access, bool inherit, int pid);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool DuplicateHandle(IntPtr sourceProcess, IntPtr sourceHandle, IntPtr targetProcess, out IntPtr targetHandle, int access, bool inherit, int options);
    [DllImport("kernel32.dll")] static extern IntPtr GetCurrentProcess();
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
    [DllImport("ntdll.dll")] static extern int NtQueryObject(IntPtr handle, int infoClass, IntPtr buffer, int length, out int returnLength);

    const int SystemExtendedHandleInformation = 64;
    const int ObjectNameInformation = 1;
    const int ObjectTypeInformation = 2;
    const int PROCESS_DUP_HANDLE = 0x0040;
    const int DUPLICATE_CLOSE_SOURCE = 0x1;
    const int DUPLICATE_SAME_ACCESS = 0x2;
    const string EventName = "ROBLOX_singletonEvent";
    const string MutexName = "ROBLOX_singletonMutex";
    const int ScratchSize = 4096;

    // SYSTEM_HANDLE_INFORMATION_EX: NumberOfHandles, Reserved, then entries of
    // SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX { Object, UniqueProcessId,
    // HandleValue, GrantedAccess(4), CreatorBackTraceIndex(2),
    // ObjectTypeIndex(2), HandleAttributes(4), Reserved(4) }.
    static readonly int HeaderSize = IntPtr.Size * 2;
    static readonly int EntrySize = IntPtr.Size * 3 + 16;
    static readonly int PidOffset = IntPtr.Size;
    static readonly int HandleOffset = IntPtr.Size * 2;
    static readonly int TypeIndexOffset = IntPtr.Size * 3 + 6;

    static readonly object _lock = new object();
    static readonly SystemInfoBuffer _table = new SystemInfoBuffer(SystemExtendedHandleInformation, 4 << 20);
    static readonly Dictionary<int, bool> _typeMayCarryName = new Dictionary<int, bool>();
    static IntPtr _scratch;

    // Returns how many singleton-event handles were closed. Deliberately
    // writes nothing to stdout -- daemon mode multiplexes a line protocol
    // over that stream, and a stray "CLOSED:<pid>" would desync it.
    //
    // exemptPids: PIDs of Roblox instances we launched and are actively
    // tracking. Their singleton handles are left alone so closing handles
    // before a new spawn can't crash an already-running instance.
    // Passing null closes handles from every Roblox process (original behaviour).
    //
    // includeMutex also closes ROBLOX_singletonMutex handles, for when a client
    // owns that mutex and it can't otherwise be acquired (see MutexHolder.Acquire).
    public static int CloseRobloxSingletonHandles(HashSet<int> exemptPids, bool includeMutex = false)
    {
        // Serialized: the startup/re-hold scan and a pre-launch request can
        // overlap, and they share the buffers and the type cache.
        lock (_lock)
        {
            int[] pids = RobloxProcesses.Snapshot();
            if (pids == null) throw new InvalidOperationException("could not enumerate Roblox processes");

            var targets = new HashSet<int>();
            foreach (int pid in pids)
                if (exemptPids == null || !exemptPids.Contains(pid)) targets.Add(pid);
            if (targets.Count == 0) return 0;

            try
            {
                if (!_table.Query()) throw new InvalidOperationException("could not read the system handle table");
                if (_scratch == IntPtr.Zero) _scratch = Marshal.AllocHGlobal(ScratchSize);
                return Scan(targets, includeMutex);
            }
            finally
            {
                // The table is tens of MB on a busy machine and this runs once
                // per launch, not per tick: keep the learned size, not the memory.
                _table.Trim();
            }
        }
    }

    static int Scan(HashSet<int> targets, bool includeMutex)
    {
        int closed = 0;
        IntPtr self = GetCurrentProcess(); // pseudo-handle, never closed
        var processes = new Dictionary<int, IntPtr>();
        try
        {
            long count = Marshal.ReadIntPtr(_table.Pointer).ToInt64();
            long capacity = (_table.Size - HeaderSize) / EntrySize;
            if (count > capacity) count = capacity;
            long entries = _table.Pointer.ToInt64() + HeaderSize;

            for (long i = 0; i < count; i++)
            {
                IntPtr entry = new IntPtr(entries + i * EntrySize);

                int pid = (int)Marshal.ReadIntPtr(entry, PidOffset).ToInt64();
                if (!targets.Contains(pid)) continue;

                int typeIndex = (ushort)Marshal.ReadInt16(entry, TypeIndexOffset);
                bool mayCarryName;
                bool typeKnown = _typeMayCarryName.TryGetValue(typeIndex, out mayCarryName);
                if (typeKnown && !mayCarryName) continue;

                IntPtr process = OpenOnce(processes, pid);
                if (process == IntPtr.Zero) continue;

                IntPtr sourceHandle = Marshal.ReadIntPtr(entry, HandleOffset);
                IntPtr local;
                if (!DuplicateHandle(process, sourceHandle, self, out local, 0, false, DUPLICATE_SAME_ACCESS))
                    continue;
                try
                {
                    if (!typeKnown)
                    {
                        string typeName = QueryUnicodeString(local, ObjectTypeInformation);
                        if (typeName == null) continue; // lookup failed; don't cache a guess
                        mayCarryName = typeName == "Event" || typeName == "Mutant" || typeName == "Semaphore";
                        // The table is a snapshot: if Roblox closed this handle and
                        // the value was reused by another object before the
                        // duplicate, typeName describes that object, not the
                        // table's type index. Caching that would mark Event as
                        // "no name" for the rest of the session and silently stop
                        // closehandles from ever finding the singleton again. Only
                        // cache when the object's own TypeIndex agrees.
                        if (TypeIndexOfLastQuery() == typeIndex)
                            _typeMayCarryName[typeIndex] = mayCarryName;
                        if (!mayCarryName) continue;
                    }

                    string name = QueryUnicodeString(local, ObjectNameInformation);
                    if (name == null) continue;
                    bool isTarget = name.IndexOf(EventName, StringComparison.Ordinal) >= 0
                        || (includeMutex && name.IndexOf(MutexName, StringComparison.Ordinal) >= 0);
                    if (!isTarget) continue;

                    IntPtr ignored;
                    if (DuplicateHandle(process, sourceHandle, IntPtr.Zero, out ignored, 0, false, DUPLICATE_CLOSE_SOURCE))
                        closed++;
                }
                finally { CloseHandle(local); }
            }
        }
        finally
        {
            foreach (IntPtr h in processes.Values)
                if (h != IntPtr.Zero) CloseHandle(h);
        }
        return closed;
    }

    // One OpenProcess per process per scan. A failure is remembered too, so
    // a process we can't open isn't retried for each of its handles.
    static IntPtr OpenOnce(Dictionary<int, IntPtr> processes, int pid)
    {
        IntPtr h;
        if (!processes.TryGetValue(pid, out h))
        {
            h = OpenProcess(PROCESS_DUP_HANDLE, false, pid);
            processes[pid] = h;
        }
        return h;
    }

    // Both ObjectNameInformation and ObjectTypeInformation start with a
    // UNICODE_STRING. Null on failure (including a name too long for the
    // scratch buffer, whose contents are then undefined); "" when unnamed.
    // OBJECT_TYPE_INFORMATION.TypeIndex (a UCHAR at 0x5A on x64, Windows 8.1
    // and later) of the object last queried with ObjectTypeInformation. On
    // older systems the byte is unrelated padding, so the comparison above
    // just never matches and the cache stays empty: slower, never wrong.
    const int TypeIndexFieldOffset = 0x5A;
    static int TypeIndexOfLastQuery()
    {
        if (IntPtr.Size != 8) return -1;
        return Marshal.ReadByte(_scratch, TypeIndexFieldOffset);
    }

    static string QueryUnicodeString(IntPtr handle, int infoClass)
    {
        int returned;
        if (NtQueryObject(handle, infoClass, _scratch, ScratchSize, out returned) != 0) return null;
        int bytes = (ushort)Marshal.ReadInt16(_scratch);
        if (bytes == 0) return "";
        IntPtr chars = Marshal.ReadIntPtr(_scratch, IntPtr.Size); // Buffer follows Length/MaximumLength + padding
        if (chars == IntPtr.Zero) return "";
        return Marshal.PtrToStringUni(chars, bytes / 2);
    }
}

// ── OS-level Roblox volume (ported from audiovol.ps1) ───────────────────────
// Sets the per-session volume of every audio session on the default render
// device that belongs to a Roblox process. Every COM object is released
// explicitly: this runs on each slider change, and leaving them to the
// finalizer kept audio-session references alive long after the call.
internal static class AudioVolume
{
    [ComImport, Guid("BCDE0395-E52F-467C-8E3D-C4579291692E")] class MMDeviceEnumerator { }

    [Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IMMDeviceEnumerator
    {
        int EnumAudioEndpoints(int dataFlow, int stateMask, out IntPtr devices);
        int GetDefaultAudioEndpoint(int dataFlow, int role, out IMMDevice device);
    }

    [Guid("D666063F-1587-4E43-81F1-B948E807363F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IMMDevice
    {
        int Activate(ref Guid iid, int clsCtx, IntPtr activationParams, [MarshalAs(UnmanagedType.IUnknown)] out object iface);
    }

    [Guid("77AA99A0-1BD6-484F-8BC7-2C654C9A9B6F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IAudioSessionManager2
    {
        // IAudioSessionManager's two methods, unused but they hold vtable slots.
        int GetAudioSessionControl();
        int GetSimpleAudioVolume();
        int GetSessionEnumerator(out IAudioSessionEnumerator enumerator);
    }

    [Guid("E2F5BB11-0570-40CA-ACDD-3AA01277DEE8"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IAudioSessionEnumerator
    {
        int GetCount(out int count);
        int GetSession(int index, out IAudioSessionControl session);
    }

    [Guid("F4B1A599-7266-4319-A8CA-E70ACB11E8CD"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IAudioSessionControl
    {
        int GetState(out int state);
        int GetDisplayName(out IntPtr name);
        int SetDisplayName(string value, ref Guid ctx);
        int GetIconPath(out IntPtr path);
        int SetIconPath(string value, ref Guid ctx);
        int GetGroupingParam(out Guid param);
        int SetGroupingParam(ref Guid over, ref Guid ctx);
        int RegisterAudioSessionNotification(IntPtr n);
        int UnregisterAudioSessionNotification(IntPtr n);
    }

    [Guid("BFB7FF88-7239-4FC9-8FA2-07C950BE9C6D"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IAudioSessionControl2
    {
        // 9 inherited IAudioSessionControl methods (must be present so the
        // derived methods land at the correct vtable slots).
        int R1(); int R2(); int R3(); int R4(); int R5();
        int R6(); int R7(); int R8(); int R9();
        int GetSessionIdentifier(out IntPtr id);
        int GetSessionInstanceIdentifier(out IntPtr id);
        int GetProcessId(out int pid);
    }

    [Guid("87CE5498-68D6-44E5-9215-6DA47EF883D8"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface ISimpleAudioVolume
    {
        int SetMasterVolume(float level, ref Guid eventContext);
        int GetMasterVolume(out float level);
        int SetMute(bool mute, ref Guid eventContext);
        int GetMute(out bool mute);
    }

    const int eRender = 0;
    const int eConsole = 0;
    const int CLSCTX_ALL = 0x17;
    static readonly Guid IID_IAudioSessionManager2 = new Guid("77AA99A0-1BD6-484F-8BC7-2C654C9A9B6F");

    public static int Apply(float level, HashSet<int> pids)
    {
        object enumeratorObj = null;
        IMMDevice device = null;
        object managerObj = null;
        IAudioSessionEnumerator sessions = null;
        try
        {
            enumeratorObj = new MMDeviceEnumerator();
            var enumerator = (IMMDeviceEnumerator)enumeratorObj;
            if (enumerator.GetDefaultAudioEndpoint(eRender, eConsole, out device) != 0 || device == null)
                return 0;

            Guid iid = IID_IAudioSessionManager2;
            if (device.Activate(ref iid, CLSCTX_ALL, IntPtr.Zero, out managerObj) != 0 || managerObj == null)
                return 0;
            if (((IAudioSessionManager2)managerObj).GetSessionEnumerator(out sessions) != 0 || sessions == null)
                return 0;

            int count;
            if (sessions.GetCount(out count) != 0) return 0;

            Guid context = Guid.Empty;
            int changed = 0;
            for (int i = 0; i < count; i++)
            {
                IAudioSessionControl control = null;
                try
                {
                    if (sessions.GetSession(i, out control) != 0 || control == null) continue;
                    var control2 = control as IAudioSessionControl2;
                    if (control2 == null) continue;
                    int pid;
                    if (control2.GetProcessId(out pid) != 0 || !pids.Contains(pid)) continue;
                    var volume = control as ISimpleAudioVolume;
                    if (volume == null) continue;
                    if (volume.SetMasterVolume(level, ref context) == 0) changed++;
                }
                finally { ReleaseCom(control); } // one RCW backs all three casts
            }
            return changed;
        }
        finally
        {
            ReleaseCom(sessions);
            ReleaseCom(managerObj);
            ReleaseCom(device);
            ReleaseCom(enumeratorObj);
        }
    }

    static void ReleaseCom(object o)
    {
        if (o == null || !Marshal.IsComObject(o)) return;
        try { Marshal.ReleaseComObject(o); } catch { }
    }
}

// ── Window focus helpers shared by anti-AFK and capture ─────────────────────
internal static class WindowFocus
{
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumWindowsProc callback, IntPtr lParam);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] static extern bool IsWindow(IntPtr hWnd);
    [DllImport("user32.dll")] static extern int GetWindowTextLength(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] static extern bool BringWindowToTop(IntPtr hWnd);
    [DllImport("user32.dll")] static extern void SwitchToThisWindow(IntPtr hWnd, bool altTab);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr hWnd);
    [DllImport("user32.dll")] static extern bool AttachThreadInput(uint idAttach, uint idAttachTo, bool attach);
    [DllImport("kernel32.dll")] static extern uint GetCurrentThreadId();
    [DllImport("user32.dll")] static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] static extern bool SetProcessDpiAwarenessContext(IntPtr context);

    delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);

    public const int SW_RESTORE = 9;
    public const int SW_MINIMIZE = 6;
    public const int SW_SHOWNOACTIVATE = 4;
    static readonly IntPtr DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2 = new IntPtr(-4);

    // Taken by anything that moves focus around. An anti-AFK pass and a
    // capture each assume they own the foreground for a few hundred ms; run
    // together, one would screenshot the window the other just focused.
    public static readonly object Gate = new object();

    // Without this the process is DPI-virtualized: on a scaled display,
    // window rectangles come back in scaled units while CopyFromScreen copies
    // physical pixels, so captures were offset and cropped wrong.
    public static void EnableDpiAwareness()
    {
        try
        {
            if (SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)) return;
        }
        catch (EntryPointNotFoundException) { } // before Windows 10 1703
        try { SetProcessDPIAware(); } catch { }
    }

    // Force a window to the foreground, defeating Windows' foreground lock by
    // attaching to the currently-foreground thread's input queue for the call.
    public static void ForceForeground(IntPtr hWnd)
    {
        IntPtr fg = GetForegroundWindow();
        // Already in front: nothing to do. Going on would attach to the
        // target's own input queue, where BringWindowToTop runs synchronously
        // and blocks for as long as a frozen Roblox window stays frozen,
        // holding the focus gate that anti-AFK and captures share.
        if (fg == hWnd) return;
        uint thisThread = GetCurrentThreadId();
        uint ignoredPid;
        // The return value is the owning THREAD id. (The old code passed the
        // owning process id here, so the attach always failed.)
        uint fgThread = fg != IntPtr.Zero ? GetWindowThreadProcessId(fg, out ignoredPid) : 0;
        uint targetThread = GetWindowThreadProcessId(hWnd, out ignoredPid);
        bool attached = fgThread != 0 && fgThread != thisThread && fgThread != targetThread
            && AttachThreadInput(thisThread, fgThread, true);
        try
        {
            SetForegroundWindow(hWnd);
            BringWindowToTop(hWnd);
        }
        finally
        {
            if (attached) AttachThreadInput(thisThread, fgThread, false);
        }
    }

    // Focuses a window and reports whether it really took. Windows refuses focus
    // changes from background processes some of the time, so one attempt isn't
    // enough; the later attempts add the call that is refused least often.
    public static bool TryForeground(IntPtr hWnd)
    {
        for (int attempt = 0; attempt < 3; attempt++)
        {
            ForceForeground(hWnd);
            if (attempt > 0) SwitchToThisWindow(hWnd, true);
            Thread.Sleep(60 + attempt * 60);
            if (GetForegroundWindow() == hWnd) return true;
        }
        return false;
    }

    // Hands focus back to where the user was. Two attempts with a short
    // settle reliably land it, including after a minimised target was
    // re-minimised.
    public static void RestoreForeground(IntPtr hWnd)
    {
        if (hWnd == IntPtr.Zero || !IsWindow(hWnd)) return;
        try
        {
            Thread.Sleep(40);
            ForceForeground(hWnd);
            Thread.Sleep(40);
            SetForegroundWindow(hWnd); // second pass; focus has settled by now
        }
        catch { }
    }

    // pid -> first visible, titled top-level window (the game window) for
    // each of the given processes.
    public static void FindMainWindows(List<int> pids, Dictionary<uint, IntPtr> into)
    {
        into.Clear();
        if (pids.Count == 0) return;
        var wanted = new HashSet<uint>();
        foreach (int pid in pids) wanted.Add((uint)pid);
        EnumWindows(delegate(IntPtr hWnd, IntPtr lParam)
        {
            if (!IsWindowVisible(hWnd)) return true;
            if (GetWindowTextLength(hWnd) == 0) return true; // main game window has a title
            uint pid;
            GetWindowThreadProcessId(hWnd, out pid);
            if (wanted.Contains(pid) && !into.ContainsKey(pid)) into[pid] = hWnd;
            return true;
        }, IntPtr.Zero);
    }

    [StructLayout(LayoutKind.Sequential)]
    struct WinRect { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hWnd, out WinRect rect);

    // The game window of one process: its largest visible, titled top-level
    // window. Process.MainWindowHandle takes the first visible unowned window
    // it meets, whatever that is, so a capture could land on the wrong one.
    public static IntPtr LargestWindowOf(int targetPid)
    {
        IntPtr best = IntPtr.Zero;
        long bestArea = -1;
        EnumWindows(delegate(IntPtr hWnd, IntPtr lParam)
        {
            uint pid;
            GetWindowThreadProcessId(hWnd, out pid);
            if (pid != (uint)targetPid || !IsWindowVisible(hWnd) || GetWindowTextLength(hWnd) == 0) return true;
            WinRect r;
            if (!GetWindowRect(hWnd, out r)) return true;
            long area = (long)(r.Right - r.Left) * (r.Bottom - r.Top);
            if (area > bestArea) { bestArea = area; best = hWnd; }
            return true;
        }, IntPtr.Zero);
        return best;
    }
}

// ── Foreground lock timeout override ────────────────────────────────────────
// SetForegroundWindow from a background process is refused while the
// system's foreground lock timeout is running, so anti-AFK sets it to zero
// for as long as it runs. This is a SYSTEM-WIDE setting that applies to
// every app until the next reboot, so the old value is read first and put
// back when the last user lets go -- including from a ProcessExit hook, as
// a backstop for an exit that didn't wait for the anti-AFK thread.
//
// It is set once per anti-AFK run rather than per pass on purpose: the call
// only succeeds while this process may change the foreground, which is
// typically true when the user has just toggled anti-AFK in Vanta Manager and
// typically false 18 minutes later mid-game.
internal static class ForegroundLockTimeout
{
    [DllImport("user32.dll")] static extern bool SystemParametersInfo(uint action, uint param, out uint value, uint winIni);
    [DllImport("user32.dll")] static extern bool SystemParametersInfo(uint action, uint param, IntPtr value, uint winIni);

    const uint SPI_GETFOREGROUNDLOCKTIMEOUT = 0x2000;
    const uint SPI_SETFOREGROUNDLOCKTIMEOUT = 0x2001;

    static readonly object _lock = new object();
    static int _users;
    static bool _overridden;
    static uint _saved;
    static bool _exitHooked;

    public static void Acquire()
    {
        lock (_lock)
        {
            _users++;
            if (_users > 1) return;
            try
            {
                uint previous;
                if (!SystemParametersInfo(SPI_GETFOREGROUNDLOCKTIMEOUT, 0, out previous, 0) || previous == 0)
                    return; // unreadable, or already zero: nothing to change or restore
                // For this action the new value travels *in* pvParam, it is
                // not a pointer to it -- zero here means "no delay".
                if (!SystemParametersInfo(SPI_SETFOREGROUNDLOCKTIMEOUT, 0, IntPtr.Zero, 0)) return;
                _saved = previous;
                _overridden = true;
                if (!_exitHooked)
                {
                    _exitHooked = true;
                    AppDomain.CurrentDomain.ProcessExit += delegate { RestoreLocked(); };
                }
            }
            catch { }
        }
    }

    public static void Release()
    {
        lock (_lock)
        {
            if (_users > 0) _users--;
            if (_users == 0) RestoreLocked();
        }
    }

    static void RestoreLocked()
    {
        lock (_lock)
        {
            if (!_overridden) return;
            _overridden = false;
            try { SystemParametersInfo(SPI_SETFOREGROUNDLOCKTIMEOUT, 0, new IntPtr((long)_saved), 0); }
            catch { }
        }
    }
}

// ── Hidden windows ──────────────────────────────────────────────────────────
// "Hidden" accounts: the window is minimized first (a minimized Roblox draws
// nothing) and then hidden, so it has no window and no taskbar button either.
// The window keeps its minimized state while hidden. Only ShowWindow is used,
// the same call the taskbar makes; nothing is read from or put into the client.
//
// Other code only looks for visible windows, so the ones hidden here are
// remembered: anti-AFK and captures show them for a moment and hide them again,
// and every one is put back on screen (minimized) when this helper exits.
internal static class HiddenWindows
{
    [DllImport("user32.dll")] static extern bool IsWindow(IntPtr hWnd);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);

    delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc callback, IntPtr lParam);
    [DllImport("user32.dll")] static extern int GetWindowTextLength(IntPtr hWnd);

    const int SW_HIDE = 0;
    const int SW_SHOWMINNOACTIVE = 7;

    static readonly object _lock = new object();
    static readonly Dictionary<int, IntPtr> _byPid = new Dictionary<int, IntPtr>();

    // The remembered window of pid, if it still belongs to that process.
    static IntPtr Valid(int pid)
    {
        IntPtr h;
        if (!_byPid.TryGetValue(pid, out h)) return IntPtr.Zero;
        uint owner;
        if (!IsWindow(h) || GetWindowThreadProcessId(h, out owner) == 0 || owner != (uint)pid)
        {
            _byPid.Remove(pid); // closed: nothing to show any more
            return IntPtr.Zero;
        }
        return h;
    }

    public static IntPtr Of(int pid)
    {
        lock (_lock) return Valid(pid);
    }

    // A window hiding left behind that this helper no longer remembers (it was
    // restarted since): invisible, minimized, titled, and owned by the process.
    // Only used when someone asks for the window back, so an unrelated hidden
    // window of Roblox is never touched on its own.
    static IntPtr FindStranded(int pid)
    {
        IntPtr found = IntPtr.Zero;
        EnumWindows(delegate(IntPtr h, IntPtr l)
        {
            uint owner;
            GetWindowThreadProcessId(h, out owner);
            if (owner != (uint)pid || IsWindowVisible(h) || GetWindowTextLength(h) == 0 || !WindowFocus.IsIconic(h)) return true;
            found = h;
            return false;
        }, IntPtr.Zero);
        return found;
    }

    public static bool Hide(int pid)
    {
        lock (WindowFocus.Gate)
        lock (_lock)
        {
            IntPtr h = WindowFocus.LargestWindowOf(pid);
            if (h == IntPtr.Zero)
            {
                // Already hidden by us (Roblox hasn't shown it again): done.
                IntPtr known = Valid(pid);
                if (known == IntPtr.Zero)
                {
                    known = FindStranded(pid);
                    if (known != IntPtr.Zero) _byPid[pid] = known; // remembered again
                }
                return known != IntPtr.Zero && !IsWindowVisible(known);
            }
            if (!WindowFocus.IsIconic(h))
            {
                WindowFocus.ShowWindow(h, SW_SHOWMINNOACTIVE);
                Thread.Sleep(120);
            }
            WindowFocus.ShowWindow(h, SW_HIDE);
            _byPid[pid] = h;
            return true;
        }
    }

    public static bool Show(int pid)
    {
        lock (WindowFocus.Gate)
        {
            IntPtr h;
            lock (_lock)
            {
                h = Valid(pid);
                _byPid.Remove(pid);
            }
            if (h == IntPtr.Zero) h = WindowFocus.LargestWindowOf(pid);
            if (h == IntPtr.Zero) h = FindStranded(pid);
            if (h == IntPtr.Zero) return false;
            WindowFocus.ShowWindow(h, WindowFocus.SW_RESTORE);
            WindowFocus.TryForeground(h);
            return true;
        }
    }

    // Back on the taskbar but still minimized, without taking focus: for
    // showing many at once, where popping each one up would be in the way.
    public static bool Unhide(int pid)
    {
        lock (WindowFocus.Gate)
        {
            IntPtr h;
            lock (_lock)
            {
                h = Valid(pid);
                _byPid.Remove(pid);
            }
            if (h == IntPtr.Zero) h = FindStranded(pid);
            if (h == IntPtr.Zero) return false;
            WindowFocus.ShowWindow(h, SW_SHOWMINNOACTIVE);
            return true;
        }
    }

    // Anti-AFK's list of windows, plus the hidden ones of the given processes.
    public static void AddTo(List<int> pids, Dictionary<uint, IntPtr> windows)
    {
        lock (_lock)
        {
            foreach (int pid in pids)
            {
                if (windows.ContainsKey((uint)pid)) continue;
                IntPtr h = Valid(pid);
                if (h != IntPtr.Zero) windows[(uint)pid] = h;
            }
        }
    }

    // After a window was shown for a tap or a capture: hidden again if it's one of ours.
    public static void HideAgainIfHidden(IntPtr hWnd)
    {
        bool ours;
        lock (_lock) ours = _byPid.ContainsValue(hWnd);
        if (!ours || !IsWindow(hWnd)) return;
        if (!WindowFocus.IsIconic(hWnd)) WindowFocus.ShowWindow(hWnd, SW_SHOWMINNOACTIVE);
        WindowFocus.ShowWindow(hWnd, SW_HIDE);
    }

    // On exit: every hidden window back on the taskbar, minimized.
    public static void ShowAll()
    {
        lock (_lock)
        {
            foreach (var kv in _byPid)
                if (IsWindow(kv.Value)) WindowFocus.ShowWindow(kv.Value, SW_SHOWMINNOACTIVE);
            _byPid.Clear();
        }
    }
}

// ── Anti-AFK input ──────────────────────────────────────────────────────────
// Roblox only registers input while its window is focused, so a background
// PostMessage is ignored by unfocused instances. To keep EVERY instance alive,
// each Roblox window is briefly foregrounded in turn (restoring it first if it
// was minimised), given a real key tap via keybd_event, then put back. The
// originally-focused window is restored after each pass. Per-instance timers
// mean each account is tapped the moment IT reaches the deadline (instances
// launched at different times have independent countdowns) -- including
// whichever one you're currently playing, since being focused doesn't by
// itself prove Roblox saw real input. Default deadline is 18 min -- safely
// under Roblox's ~20-minute idle kick.
internal static class AntiAfk
{
    [DllImport("user32.dll")] static extern uint MapVirtualKey(uint code, uint mapType);
    [DllImport("user32.dll")] static extern void keybd_event(byte vk, byte scan, uint flags, IntPtr extraInfo);

    // VK_SHIFT: registers as input, moves nothing, opens no chat.
    public const int DefaultVirtualKey = 0x10;
    const uint KEYEVENTF_KEYUP = 0x0002;
    const uint KEYEVENTF_EXTENDEDKEY = 0x0001;
    // Fire within ~15s of the deadline, but wake immediately on stop.
    const int PassIntervalMs = 15 * 1000;

    // onTap reports each tapped PID (daemon mode turns that into an event
    // line). stop lets daemon mode reconfigure or switch anti-AFK off without
    // killing the process.
    public static void RunLoop(int deadlineSec, byte vk, Action<uint> onTap, Action<uint> onStuck, WaitHandle stop)
    {
        ForegroundLockTimeout.Acquire();
        try
        {
            RunLoopCore(deadlineSec * 1000L, vk, onTap, onStuck, stop);
        }
        finally
        {
            ForegroundLockTimeout.Release();
        }
    }

    static void RunLoopCore(long deadlineMs, byte vk, Action<uint> onTap, Action<uint> onStuck, WaitHandle stop)
    {
        byte scan = (byte)MapVirtualKey(vk, 0);
        var rng = new Random();
        // Monotonic, so a wall-clock adjustment can't skip or bunch up taps.
        var clock = Stopwatch.StartNew();
        // pid -> clock time its idle timer last reset (launch or our tap)
        var lastReset = new Dictionary<uint, long>();
        var failures = new Dictionary<uint, int>();
        var pids = new List<int>();
        var windows = new Dictionary<uint, IntPtr>();
        var due = new List<uint>();
        var gone = new List<uint>();

        while (!stop.WaitOne(PassIntervalMs))
        {
            // One bad pass must not end anti-AFK (or, before the worker
            // caught it, the whole process).
            try
            {
                if (!RobloxProcesses.Find(pids)) continue;
                WindowFocus.FindMainWindows(pids, windows);
                HiddenWindows.AddTo(pids, windows);

                long now = clock.ElapsedMilliseconds;
                gone.Clear();
                foreach (uint pid in lastReset.Keys) if (!windows.ContainsKey(pid)) gone.Add(pid);
                foreach (uint pid in gone) { lastReset.Remove(pid); failures.Remove(pid); }
                // A window seen for the first time may already have been idle for
                // a long while (anti-AFK switched on mid-session), so it gets a
                // tap soon instead of a full interval from now.
                foreach (uint pid in windows.Keys)
                    if (!lastReset.ContainsKey(pid)) lastReset[pid] = now - Math.Max(0, deadlineMs - FirstTapAfterMs);

                // Foreground state alone doesn't prove Roblox saw real input --
                // being focused without pressing anything still reads as idle
                // to Roblox's own kick timer, so exempting the foreground
                // window let it get missed and kicked anyway. Every window is
                // tapped on its own deadline regardless of focus; the tap is a
                // harmless single key and focus is handed back right after.
                due.Clear();
                foreach (var kv in windows)
                    if (now - lastReset[kv.Key] >= deadlineMs) due.Add(kv.Key);
                if (due.Count == 0) continue;

                TapAll(due, windows, lastReset, failures, clock, deadlineMs, vk, scan, rng, onTap, onStuck, stop);
            }
            catch (Exception ex)
            {
                Protocol.Warn("AntiAfk: " + ex.Message);
            }
        }
    }

    const long RetryAfterFailMs = 15000;
    const long FirstTapAfterMs = 90000;
    // Reported every this many failed taps in a row, so a stuck window is
    // visible in the log without a line every retry.
    const int ReportEveryFailures = 4;

    static void TapAll(List<uint> due, Dictionary<uint, IntPtr> windows, Dictionary<uint, long> lastReset, Dictionary<uint, int> failures,
                       Stopwatch clock, long deadlineMs, byte vk, byte scan, Random rng, Action<uint> onTap, Action<uint> onStuck, WaitHandle stop)
    {
        lock (WindowFocus.Gate)
        {
            // Capture the window you're on BEFORE touching anything, so focus
            // always goes back exactly where it was -- on every exit path.
            IntPtr originalFg = WindowFocus.GetForegroundWindow();
            try
            {
                foreach (uint pid in due)
                {
                    if (stop.WaitOne(0)) break;
                    bool tapped = TapWindow(windows[pid], vk, scan, rng);
                    // Only a real tap resets the idle clock. A failed one is
                    // retried in RetryAfterFailMs rather than a whole interval
                    // later (too late to beat the kick), but not on every pass,
                    // which would keep yanking focus around.
                    long t = clock.ElapsedMilliseconds;
                    lastReset[pid] = tapped ? t : t - Math.Max(0, deadlineMs - RetryAfterFailMs);
                    if (tapped)
                    {
                        failures[pid] = 0;
                        if (onTap != null) onTap(pid);
                    }
                    else
                    {
                        int count;
                        failures.TryGetValue(pid, out count);
                        failures[pid] = ++count;
                        if (count % ReportEveryFailures == 0 && onStuck != null) onStuck(pid);
                    }
                }
            }
            finally
            {
                WindowFocus.RestoreForeground(originalFg);
            }
        }
    }

    static bool IsExtendedKey(byte vk)
    {
        return (vk >= 0x21 && vk <= 0x28) || vk == 0x2D || vk == 0x2E;
    }

    // Focus one window and send a single benign key tap. Does NOT restore the
    // previous foreground (TapAll does that once after all due taps).
    static bool TapWindow(IntPtr hWnd, byte vk, byte scan, Random rng)
    {
        bool wasMinimised = WindowFocus.IsIconic(hWnd);
        try
        {
            if (wasMinimised) WindowFocus.ShowWindow(hWnd, WindowFocus.SW_RESTORE);
            // keybd_event goes to whatever has focus. If the switch doesn't
            // take (window closed, foreground lock refused), the tap would land
            // in the user's own app instead of Roblox, so skip it.
            if (!WindowFocus.TryForeground(hWnd)) return false;
            // Arrows, Home/End, Page Up/Down, Insert and Delete are "extended" keys; sent
            // without the flag they arrive as the number pad's keys instead.
            uint ext = IsExtendedKey(vk) ? KEYEVENTF_EXTENDEDKEY : 0;
            Thread.Sleep(20 + rng.Next(40));               // slight jitter
            keybd_event(vk, scan, ext, IntPtr.Zero);       // key down
            Thread.Sleep(35 + rng.Next(40));
            keybd_event(vk, scan, KEYEVENTF_KEYUP | ext, IntPtr.Zero); // key up
            Thread.Sleep(30 + rng.Next(30));
            return true;
        }
        catch { return false; }
        finally
        {
            if (wasMinimised) WindowFocus.ShowWindow(hWnd, WindowFocus.SW_MINIMIZE);
            HiddenWindows.HideAgainIfHidden(hWnd);
        }
    }
}

// ── Window layout ───────────────────────────────────────────────────────────
// Lines Roblox windows up as small windows in a gap-free grid on the work area
// of the monitor the first one is on, and puts them back as they were.
//
// Roblox holds its window to a minimum client area (816 x 638, measured) for as
// long as the window has a sizing frame. A borderless popup isn't held to it, so
// each resize swaps to that style for the one SetWindowPos and puts the real
// style straight back. That approach and the measured minimum come from RoALT's
// window.rs (free_resize), MIT licensed, Copyright (c) 2026 inspectsoftware.
//
// Roblox grows its window back to the minimum after a minimize and restore, so a
// slow keeper thread puts a tiled window's size right again (never its place).
// Only window calls are used (SetWindowPos and friends, as dragging a window's
// edge does); nothing is injected into or read from the client.
internal static class WindowLayout
{
    [StructLayout(LayoutKind.Sequential)] struct RECT { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] struct POINT { public int X, Y; }
    [StructLayout(LayoutKind.Sequential)]
    struct WINDOWPLACEMENT
    {
        public int length, flags, showCmd;
        public POINT ptMinPosition, ptMaxPosition;
        public RECT rcNormalPosition;
    }
    [StructLayout(LayoutKind.Sequential)]
    struct MONITORINFO { public int cbSize; public RECT rcMonitor, rcWork; public int dwFlags; }

    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hwnd, out RECT r);
    [DllImport("user32.dll")] static extern bool GetClientRect(IntPtr hwnd, out RECT r);
    [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr hwnd, int attribute, out RECT value, int size);
    [DllImport("user32.dll")] static extern bool SetWindowPos(IntPtr hwnd, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")] static extern IntPtr GetWindowLongPtr(IntPtr hwnd, int index);
    [DllImport("user32.dll", EntryPoint = "SetWindowLongPtrW")] static extern IntPtr SetWindowLongPtr(IntPtr hwnd, int index, IntPtr value);
    [DllImport("user32.dll")] static extern bool GetWindowPlacement(IntPtr hwnd, ref WINDOWPLACEMENT placement);
    [DllImport("user32.dll")] static extern bool SetWindowPlacement(IntPtr hwnd, ref WINDOWPLACEMENT placement);
    [DllImport("user32.dll")] static extern IntPtr MonitorFromWindow(IntPtr hwnd, uint flags);
    [DllImport("user32.dll")] static extern bool GetMonitorInfo(IntPtr monitor, ref MONITORINFO info);
    [DllImport("user32.dll")] static extern bool IsWindow(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool IsHungAppWindow(IntPtr hwnd);
    [DllImport("user32.dll")] static extern short GetAsyncKeyState(int vk);

    const int GWL_STYLE = -16;
    const long WS_POPUP = 0x80000000L, WS_CAPTION = 0xC00000L, WS_THICKFRAME = 0x40000L, WS_SYSMENU = 0x80000L;
    const long WS_MINIMIZEBOX = 0x20000L, WS_MAXIMIZEBOX = 0x10000L, WS_MAXIMIZE = 0x1000000L;
    const uint SWP_NOSIZE = 0x1, SWP_NOMOVE = 0x2, SWP_NOZORDER = 0x4, SWP_NOACTIVATE = 0x10, SWP_FRAMECHANGED = 0x20;
    const uint MONITOR_DEFAULTTONEAREST = 2;
    const int DWMWA_EXTENDED_FRAME_BOUNDS = 9;
    const int SW_SHOWNOACTIVATE = 4, SW_SHOWMAXIMIZED = 3;
    const int KeepEveryMs = 4000;

    // One thing at a time: a resize swaps the window's style for a moment, and a
    // second one starting inside that would save the borderless style as the real one.
    static readonly object _sync = new object();
    // Where each window was before it was tiled, and the client size it is kept at.
    static readonly Dictionary<IntPtr, WINDOWPLACEMENT> _before = new Dictionary<IntPtr, WINDOWPLACEMENT>();
    static readonly Dictionary<IntPtr, int[]> _kept = new Dictionary<IntPtr, int[]>();
    // Which cell (counting from the corner) each tiled window has, so a new one can take a free cell.
    static readonly Dictionary<IntPtr, int> _slot = new Dictionary<IntPtr, int>();

    static long StyleOf(IntPtr hwnd) { return GetWindowLongPtr(hwnd, GWL_STYLE).ToInt64() & 0xFFFFFFFFL; }
    static void SetStyle(IntPtr hwnd, long style) { SetWindowLongPtr(hwnd, GWL_STYLE, new IntPtr(style)); }
    static long Borderless(long style)
    {
        return (style & ~(WS_CAPTION | WS_THICKFRAME | WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX)) | WS_POPUP;
    }

    // Sets the window's outer size (and place, unless keepPlace) with the style
    // swapped for the one call; the real style, frame and all, goes straight back.
    static void FreeResize(IntPtr hwnd, int x, int y, int w, int h, bool keepPlace)
    {
        long style = StyleOf(hwnd);
        SetStyle(hwnd, Borderless(style));
        try
        {
            SetWindowPos(hwnd, IntPtr.Zero, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE | (keepPlace ? SWP_NOMOVE : 0));
        }
        finally
        {
            SetStyle(hwnd, style);
            SetWindowPos(hwnd, IntPtr.Zero, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
        }
    }

    // Makes one window's game area cw x ch where it stands, for an account set
    // to start small. A maximized window is restored first. False when the
    // window can't be resized right now (minimized, hung or gone).
    public static bool ShrinkOne(IntPtr hwnd, int cw, int ch)
    {
        lock (_sync)
        {
            if (!IsWindow(hwnd) || WindowFocus.IsIconic(hwnd) || IsHungAppWindow(hwnd)) return false;
            cw = Math.Max(160, Math.Min(cw, 1280));
            ch = Math.Max(90, Math.Min(ch, 720));
            if ((StyleOf(hwnd) & WS_MAXIMIZE) != 0) WindowFocus.ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            RECT win, client;
            if (!GetWindowRect(hwnd, out win) || !GetClientRect(hwnd, out client)) return false;
            int frameW = Math.Max(0, (win.Right - win.Left) - (client.Right - client.Left));
            int frameH = Math.Max(0, (win.Bottom - win.Top) - (client.Bottom - client.Top));
            FreeResize(hwnd, 0, 0, cw + frameW, ch + frameH, true);
            return true;
        }
    }

    // Lays the windows of these processes out in rows of cw x ch client areas, `gap`
    // pixels apart (0: touching), starting from a corner of the work area (0 top left,
    // 1 top right, 2 bottom left, 3 bottom right); more than fit start over at that
    // corner. Minimized and unresponsive windows are left alone. With `append`, windows that
    // are already tiled stay where they are (even if you moved them) and only the others take
    // the free cells. Returns how many were placed.
    public static int Tile(List<int> pids, int cw, int ch, int corner, int gap, bool append)
    {
        lock (_sync)
        {
            var wins = new List<IntPtr>();
            foreach (int pid in pids)
            {
                IntPtr h = WindowFocus.LargestWindowOf(pid);
                if (h == IntPtr.Zero || wins.Contains(h) || WindowFocus.IsIconic(h) || IsHungAppWindow(h)) continue;
                if (append && _kept.ContainsKey(h)) continue;
                wins.Add(h);
            }
            if (wins.Count == 0) return 0;

            IntPtr first = wins[0];
            RECT win, client;
            if (!GetWindowRect(first, out win) || !GetClientRect(first, out client)) return 0;
            // What the window adds around its client area (title bar and edges): the
            // same for every Roblox window, since they share a style.
            int frameW = Math.Max(0, (win.Right - win.Left) - (client.Right - client.Left));
            int frameH = Math.Max(0, (win.Bottom - win.Top) - (client.Bottom - client.Top));
            // Windows also draws an invisible resize border outside what you see.
            // Pulling the cells together by it makes the visible edges touch.
            int insetL = 0, insetT = 0, insetR = 0, insetB = 0;
            RECT seen;
            if (DwmGetWindowAttribute(first, DWMWA_EXTENDED_FRAME_BOUNDS, out seen, Marshal.SizeOf(typeof(RECT))) == 0)
            {
                insetL = Math.Max(0, seen.Left - win.Left);
                insetT = Math.Max(0, seen.Top - win.Top);
                insetR = Math.Max(0, win.Right - seen.Right);
                insetB = Math.Max(0, win.Bottom - seen.Bottom);
            }
            int outerW = cw + frameW, outerH = ch + frameH;
            // The size of a cell as you see it, without the invisible border.
            int visW = Math.Max(1, outerW - insetL - insetR), visH = Math.Max(1, outerH - insetT - insetB);

            MONITORINFO mi = new MONITORINFO();
            mi.cbSize = Marshal.SizeOf(typeof(MONITORINFO));
            if (!GetMonitorInfo(MonitorFromWindow(first, MONITOR_DEFAULTTONEAREST), ref mi)) return 0;
            int areaW = mi.rcWork.Right - mi.rcWork.Left, areaH = mi.rcWork.Bottom - mi.rcWork.Top;
            int cols = Math.Max(1, (areaW + gap) / (visW + gap)), rows = Math.Max(1, (areaH + gap) / (visH + gap));
            int perScreen = cols * rows;
            bool fromRight = corner == 1 || corner == 3, fromBottom = corner == 2 || corner == 3;
            // Cells taken by tiled windows that are still there.
            var taken = new HashSet<int>();
            if (append)
                foreach (KeyValuePair<IntPtr, int> kv in _slot)
                    if (IsWindow(kv.Key) && _kept.ContainsKey(kv.Key)) taken.Add(kv.Value);

            int placed = 0;
            for (int i = 0; i < wins.Count; i++)
            {
                IntPtr h = wins[i];
                try
                {
                    if (!_before.ContainsKey(h))
                    {
                        WINDOWPLACEMENT wp = new WINDOWPLACEMENT();
                        wp.length = Marshal.SizeOf(typeof(WINDOWPLACEMENT));
                        if (GetWindowPlacement(h, ref wp)) _before[h] = wp;
                    }
                    if ((StyleOf(h) & WS_MAXIMIZE) != 0) WindowFocus.ShowWindow(h, SW_SHOWNOACTIVATE);
                    int cell = i;
                    if (append)
                    {
                        cell = 0;
                        while (taken.Contains(cell)) cell++;
                        taken.Add(cell);
                    }
                    _slot[h] = cell;
                    int slot = cell % perScreen;
                    int col = slot % cols, row = slot / cols;
                    int seenLeft = fromRight ? mi.rcWork.Right - visW - col * (visW + gap) : mi.rcWork.Left + col * (visW + gap);
                    int seenTop = fromBottom ? mi.rcWork.Bottom - visH - row * (visH + gap) : mi.rcWork.Top + row * (visH + gap);
                    int x = seenLeft - insetL, y = seenTop - insetT;
                    FreeResize(h, x, y, outerW, outerH, false);
                    _kept[h] = new int[] { cw, ch };
                    placed++;
                }
                catch (Exception ex) { Protocol.Warn("Tile: " + ex.Message); }
            }
            return placed;
        }
    }

    // Puts every tiled window back where and as it was before, and stops keeping
    // them small. Returns how many were put back.
    public static int Untile()
    {
        lock (_sync)
        {
            int back = 0;
            foreach (KeyValuePair<IntPtr, WINDOWPLACEMENT> kv in _before)
            {
                if (!IsWindow(kv.Key)) continue;
                try
                {
                    WINDOWPLACEMENT wp = kv.Value;
                    // Without taking the focus, unless it was maximized.
                    if (wp.showCmd != SW_SHOWMAXIMIZED) wp.showCmd = SW_SHOWNOACTIVATE;
                    if (SetWindowPlacement(kv.Key, ref wp)) back++;
                }
                catch (Exception ex) { Protocol.Warn("Untile: " + ex.Message); }
            }
            _before.Clear();
            _kept.Clear();
            _slot.Clear();
            return back;
        }
    }

    // Runs while windows are tiled: one that Roblox grew again (after a minimize
    // and restore, say) is made small again where it now is. A window being dragged
    // or sized by hand is left alone for that round.
    public static void KeepLoop(WaitHandle stop)
    {
        while (!stop.WaitOne(KeepEveryMs))
        {
            try { Keep(); }
            catch (Exception ex) { Protocol.Warn("Keep small: " + ex.Message); }
        }
    }

    static void Keep()
    {
        if ((GetAsyncKeyState(0x01) & 0x8000) != 0) return; // left mouse button down
        lock (_sync)
        {
            foreach (IntPtr h in new List<IntPtr>(_kept.Keys))
            {
                if (!IsWindow(h)) { _kept.Remove(h); _before.Remove(h); _slot.Remove(h); continue; }
                if (WindowFocus.IsIconic(h) || IsHungAppWindow(h)) continue;
                int[] want = _kept[h];
                RECT win, client;
                if (!GetWindowRect(h, out win) || !GetClientRect(h, out client)) continue;
                int haveW = client.Right - client.Left, haveH = client.Bottom - client.Top;
                if (Math.Abs(haveW - want[0]) <= 2 && Math.Abs(haveH - want[1]) <= 2) continue;
                int frameW = (win.Right - win.Left) - haveW, frameH = (win.Bottom - win.Top) - haveH;
                FreeResize(h, 0, 0, want[0] + frameW, want[1] + frameH, true);
            }
        }
    }
}

// ── Window preview ──────────────────────────────────────────────────────────
// A very small JPEG of what a Roblox window is showing, for the tile on its
// account. Built to cost next to nothing and to never be seen: the window is
// asked to draw itself (PrintWindow), which needs no focus and isn't affected by
// what is on top of it; nothing is ever brought to the front or restored. A
// minimized window draws nothing, so it gets "MIN" (the caller keeps the last
// picture), and a window that can't draw itself this way gets "BLANK".
internal static class WindowPreview
{
    [StructLayout(LayoutKind.Sequential)] struct RECT { public int Left, Top, Right, Bottom; }

    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hwnd, out RECT r);
    [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr hwnd, IntPtr hdc, uint flags);

    // Includes DirectX content, which a plain PrintWindow leaves black.
    const uint PW_RENDERFULLCONTENT = 2;
    const int MaxWindowSide = 16384;

    public static string Jpeg(int pid, int maxWidth, int quality)
    {
        IntPtr hwnd = WindowFocus.LargestWindowOf(pid);
        if (hwnd == IntPtr.Zero && HiddenWindows.Of(pid) != IntPtr.Zero) return "MIN";
        if (hwnd == IntPtr.Zero) throw new Exception("Roblox window not found");
        if (WindowFocus.IsIconic(hwnd)) return "MIN";

        RECT r;
        if (!GetWindowRect(hwnd, out r)) throw new Exception("Could not read the window's size");
        int width = r.Right - r.Left, height = r.Bottom - r.Top;
        if (width <= 0 || height <= 0 || width > MaxWindowSide || height > MaxWindowSide) return "BLANK";

        using (var whole = new Bitmap(width, height, PixelFormat.Format24bppRgb))
        {
            bool drew;
            using (var g = Graphics.FromImage(whole))
            {
                IntPtr hdc = g.GetHdc();
                try { drew = PrintWindow(hwnd, hdc, PW_RENDERFULLCONTENT); }
                finally { g.ReleaseHdc(hdc); }
            }
            // Just the game's picture: no title bar, no edges. The blank check
            // looks at this part only; the title bar always draws, so checking
            // the whole window let a white game area through as a real picture.
            Rectangle area = WindowCapture.ClientAreaIn(hwnd, r.Left, r.Top, width, height);
            if (!drew || WindowCapture.IsBlank(whole, area)) return "BLANK";

            int tw = Math.Max(8, Math.Min(maxWidth, area.Width));
            int th = Math.Max(8, (int)((long)area.Height * tw / area.Width));
            using (var small = new Bitmap(tw, th, PixelFormat.Format24bppRgb))
            {
                using (var g = Graphics.FromImage(small))
                {
                    g.InterpolationMode = System.Drawing.Drawing2D.InterpolationMode.HighQualityBilinear;
                    g.DrawImage(whole, new Rectangle(0, 0, tw, th), area, GraphicsUnit.Pixel);
                }
                ImageCodecInfo jpeg = null;
                foreach (ImageCodecInfo codec in ImageCodecInfo.GetImageEncoders())
                    if (codec.MimeType == "image/jpeg") { jpeg = codec; break; }
                if (jpeg == null) throw new Exception("No JPEG encoder");
                using (var ms = new MemoryStream())
                using (var options = new EncoderParameters(1))
                {
                    options.Param[0] = new EncoderParameter(System.Drawing.Imaging.Encoder.Quality, (long)quality);
                    small.Save(ms, jpeg, options);
                    return Convert.ToBase64String(ms.GetBuffer(), 0, (int)ms.Length);
                }
            }
        }
    }
}

// ── Window capture ──────────────────────────────────────────────────────────
// Screenshots one Roblox window (by PID), optionally cropped to a fractional
// sub-rectangle, and returns the PNG as base64 -- no temp file, so the caller
// decides what to do with the bytes (save, upload, preview).
//
// Asks the window to render itself (PrintWindow) first, which needs no focus
// and isn't affected by whatever is on top of it. CopyFromScreen is only the
// fallback, because it copies the SCREEN: if the focus change is refused, it
// silently returns whatever window is covering that area, and these images go
// to a webhook. So that path checks focus really moved, and fails otherwise.
internal static class WindowCapture
{
    [StructLayout(LayoutKind.Sequential)]
    struct RECT { public int Left, Top, Right, Bottom; }

    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr hWnd, int attribute, out RECT value, int size);
    [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr hWnd, IntPtr hdc, uint flags);
    [StructLayout(LayoutKind.Sequential)] struct POINT { public int X, Y; }
    [DllImport("user32.dll")] static extern bool GetClientRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] static extern bool ClientToScreen(IntPtr hWnd, ref POINT p);

    const int DWMWA_EXTENDED_FRAME_BOUNDS = 9;
    // Includes DirectX content, which a plain PrintWindow leaves black.
    const uint PW_RENDERFULLCONTENT = 2;
    // Bitmap dimensions above this are a corrupt rect, not a real window.
    const int MaxWindowSide = 16384;

    // Throws with a user-facing message on failure so daemon mode can forward
    // it verbatim as an ERR payload.
    public static string CaptureBase64(int pid, bool hasCrop, double xFrac, double yFrac, double wFrac, double hFrac)
    {
        IntPtr hwnd = MainWindowOf(pid);

        lock (WindowFocus.Gate)
        {
            IntPtr previousFg = WindowFocus.GetForegroundWindow();
            bool wasMinimised = WindowFocus.IsIconic(hwnd);
            bool tookFocus = false;
            try
            {
                if (wasMinimised)
                {
                    // Restored without activating, and given a moment to draw:
                    // a minimised window has nothing to render.
                    WindowFocus.ShowWindow(hwnd, WindowFocus.SW_SHOWNOACTIVATE);
                    Thread.Sleep(300);
                }

                Rectangle area = CaptureArea(hwnd, hasCrop, xFrac, yFrac, wFrac, hFrac);
                Bitmap image = RenderWindow(hwnd, area);
                if (image == null)
                {
                    tookFocus = true;
                    image = CopyFromScreen(hwnd, hasCrop, xFrac, yFrac, wFrac, hFrac);
                }
                using (image)
                using (var ms = new MemoryStream())
                {
                    image.Save(ms, ImageFormat.Png);
                    return Convert.ToBase64String(ms.GetBuffer(), 0, (int)ms.Length);
                }
            }
            finally
            {
                if (wasMinimised) WindowFocus.ShowWindow(hwnd, WindowFocus.SW_MINIMIZE);
                HiddenWindows.HideAgainIfHidden(hwnd);
                if ((tookFocus || wasMinimised) && previousFg != hwnd) WindowFocus.RestoreForeground(previousFg);
            }
        }
    }

    // The requested area as the window itself draws it, or null if it can't be
    // rendered this way (some GPU/driver combinations return an empty frame).
    static Bitmap RenderWindow(IntPtr hwnd, Rectangle area)
    {
        RECT r;
        if (!GetWindowRect(hwnd, out r)) return null;
        int width = r.Right - r.Left, height = r.Bottom - r.Top;
        if (width <= 0 || height <= 0 || width > MaxWindowSide || height > MaxWindowSide) return null;

        // PrintWindow draws the whole window including the invisible resize
        // border, whereas area comes from the visible bounds.
        var crop = new Rectangle(area.Left - r.Left, area.Top - r.Top, area.Width, area.Height);
        crop.Intersect(new Rectangle(0, 0, width, height));
        if (crop.Width <= 0 || crop.Height <= 0) return null;

        // Judged on the game area alone: the title bar always draws, so a white
        // or black game area inside a normal frame is still a failed render.
        Rectangle game = ClientAreaIn(hwnd, r.Left, r.Top, width, height);

        // A window that isn't in front (or was just restored) can hand back an
        // empty frame the first time; a couple of short retries usually get a
        // real one before falling back to the screen copy, which takes focus.
        for (int attempt = 0; attempt < 3; attempt++)
        {
            if (attempt > 0) Thread.Sleep(150);
            // 24bpp: the capture has no meaningful alpha, and dropping the
            // channel makes the PNG smaller and quicker to encode.
            using (var whole = new Bitmap(width, height, PixelFormat.Format24bppRgb))
            {
                bool drew;
                using (var g = Graphics.FromImage(whole))
                {
                    IntPtr hdc = g.GetHdc();
                    try { drew = PrintWindow(hwnd, hdc, PW_RENDERFULLCONTENT); }
                    finally { g.ReleaseHdc(hdc); }
                }
                if (drew && !IsBlank(whole, game)) return whole.Clone(crop, PixelFormat.Format24bppRgb);
            }
        }
        return null;
    }

    // Where the game's picture (the client area) sits inside a bitmap of the
    // whole window whose top-left is at (left, top) on screen. The whole
    // bitmap if the window won't say.
    internal static Rectangle ClientAreaIn(IntPtr hwnd, int left, int top, int width, int height)
    {
        Rectangle all = new Rectangle(0, 0, width, height);
        RECT c;
        POINT origin = new POINT();
        if (!GetClientRect(hwnd, out c) || !ClientToScreen(hwnd, ref origin)) return all;
        Rectangle client = new Rectangle(origin.X - left, origin.Y - top, c.Right - c.Left, c.Bottom - c.Top);
        client.Intersect(all);
        return client.Width > 0 && client.Height > 0 ? client : all;
    }

    internal static bool IsBlank(Bitmap image)
    {
        return IsBlank(image, new Rectangle(0, 0, image.Width, image.Height));
    }

    // A failed render comes back as one flat colour (usually white or black).
    // Sampling a grid is enough to tell, and cheap next to encoding the PNG.
    // A small tolerance, because a failed frame isn't always exactly uniform.
    internal static bool IsBlank(Bitmap image, Rectangle area)
    {
        const int Steps = 16, Tolerance = 6;
        Color first = image.GetPixel(area.Left, area.Top);
        for (int y = 0; y < Steps; y++)
        {
            for (int x = 0; x < Steps; x++)
            {
                Color c = image.GetPixel(area.Left + x * (area.Width - 1) / (Steps - 1), area.Top + y * (area.Height - 1) / (Steps - 1));
                if (Math.Abs(c.R - first.R) > Tolerance || Math.Abs(c.G - first.G) > Tolerance || Math.Abs(c.B - first.B) > Tolerance) return false;
            }
        }
        return true;
    }

    static Bitmap CopyFromScreen(IntPtr hwnd, bool hasCrop, double xFrac, double yFrac, double wFrac, double hFrac)
    {
        if (!WindowFocus.TryForeground(hwnd))
            throw new Exception("Could not bring the Roblox window to the front, so nothing was captured");
        // Long enough for a window that was behind others to draw a fresh frame;
        // one more try if the first copy still caught it before that.
        Rectangle area = CaptureArea(hwnd, hasCrop, xFrac, yFrac, wFrac, hFrac);
        Bitmap bitmap = null;
        for (int attempt = 0; attempt < 2; attempt++)
        {
            Thread.Sleep(attempt == 0 ? 200 : 350);
            if (bitmap != null) bitmap.Dispose();
            bitmap = new Bitmap(area.Width, area.Height, PixelFormat.Format24bppRgb);
            using (var g = Graphics.FromImage(bitmap))
            {
                g.CopyFromScreen(area.Left, area.Top, 0, 0, area.Size, CopyPixelOperation.SourceCopy);
            }
            if (!IsBlank(bitmap)) break;
        }
        return bitmap;
    }

    static IntPtr MainWindowOf(int pid)
    {
        IntPtr hwnd = WindowFocus.LargestWindowOf(pid);
        if (hwnd != IntPtr.Zero) return hwnd;
        hwnd = HiddenWindows.Of(pid);
        if (hwnd != IntPtr.Zero) return hwnd;

        Process process;
        try { process = Process.GetProcessById(pid); }
        catch { throw new Exception("Process not found"); }
        using (process)
        {
            hwnd = process.MainWindowHandle;
            if (hwnd == IntPtr.Zero) throw new Exception("Roblox window not found");
            return hwnd;
        }
    }

    // Window bounds in physical pixels, without the invisible resize border
    // GetWindowRect includes on Windows 10+ (which showed up as a strip of
    // desktop around every capture). Falls back to GetWindowRect if DWM has
    // no answer. Crop fractions are relative to these bounds.
    static Rectangle CaptureArea(IntPtr hwnd, bool hasCrop, double xFrac, double yFrac, double wFrac, double hFrac)
    {
        RECT r;
        if (DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, out r, Marshal.SizeOf(typeof(RECT))) != 0
            && !GetWindowRect(hwnd, out r))
            throw new Exception("Could not read Roblox window bounds");

        int width = r.Right - r.Left, height = r.Bottom - r.Top;
        if (width <= 0 || height <= 0) throw new Exception("Roblox window has invalid bounds");
        if (!hasCrop) return new Rectangle(r.Left, r.Top, width, height);

        int cx = Clamp((int)(Fraction(xFrac) * width), 0, width - 1);
        int cy = Clamp((int)(Fraction(yFrac) * height), 0, height - 1);
        int cw = Clamp((int)(Fraction(wFrac) * width), 1, width - cx);
        int ch = Clamp((int)(Fraction(hFrac) * height), 1, height - cy);
        return new Rectangle(r.Left + cx, r.Top + cy, cw, ch);
    }

    static double Fraction(double v) { return v < 0 ? 0 : (v > 1 ? 1 : v); }
    static int Clamp(int v, int min, int max) { return v < min ? min : (v > max ? max : v); }
}
