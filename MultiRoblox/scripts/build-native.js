// Precompiles src/RobloxNative.cs -> src/RobloxNative.exe using the .NET
// Framework C# compiler (csc.exe), which is present on every Windows machine.
// Runs automatically before `npm run build` (npm "prebuild" lifecycle) so a
// prebuilt native helper ships inside the app.
//
// This never hard-fails the build: if csc isn't found, or we're packaging from
// a non-Windows host, it just warns and exits 0. The app still ships
// RobloxNative.cs and compiles it once on first run as a fallback, and falls
// native helper unavailable (the related feature simply no-ops).

const fs = require('fs');
const path = require('path');
const { spawnSync } = require('child_process');

const src = path.join(__dirname, '..', 'src', 'RobloxNative.cs');
const out = path.join(__dirname, '..', 'src', 'RobloxNative.exe');

function findCsc() {
  const win = process.env.WINDIR || 'C:\\Windows';
  const candidates = [
    path.join(win, 'Microsoft.NET', 'Framework64', 'v4.0.30319', 'csc.exe'),
    path.join(win, 'Microsoft.NET', 'Framework', 'v4.0.30319', 'csc.exe'),
  ];
  return candidates.find(c => { try { return fs.existsSync(c); } catch { return false; } }) || null;
}

if (process.platform !== 'win32') {
  console.log('[build-native] non-Windows host; skipping precompile (app will compile on first run).');
  process.exit(0);
}

if (!fs.existsSync(src)) {
  console.warn('[build-native] src/RobloxNative.cs not found; skipping.');
  process.exit(0);
}

const csc = findCsc();
if (!csc) {
  console.warn('[build-native] csc.exe not found; skipping precompile (app will compile on first run).');
  process.exit(0);
}

console.log('[build-native] compiling RobloxNative.exe ...');
const r = spawnSync(csc, [
  '/nologo', '/optimize+', '/platform:x64', '/target:exe',
  '/out:' + out, src,
], { stdio: 'inherit' });

if (r.status === 0 && fs.existsSync(out)) {
  console.log('[build-native] done ->', out);
} else {
  console.warn('[build-native] compile failed (exit ' + r.status + '); app will compile on first run.');
}
process.exit(0);
