Add-Type -TypeDefinition @"
using System;
using System.Threading;

public class MutexHolder {
    public static void Hold() {
        string[] names = new string[] {
            "ROBLOX_singletonMutex",
            "ROBLOX_singletonEvent"
        };

        foreach (string name in names) {
            try {
                bool created;
                var m = new Mutex(true, name, out created);
                GC.KeepAlive(m);
            } catch (Exception) {}
        }

        Console.WriteLine("MUTEX_HELD");
        Console.Out.Flush();
        while (Console.Read() != -1) {}
    }
}
"@

[MutexHolder]::Hold()
