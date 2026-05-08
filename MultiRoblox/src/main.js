const { app, BrowserWindow, ipcMain, shell, net, session } = require('electron');

const CHROME_UA = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36';
app.commandLine.appendSwitch('user-agent', CHROME_UA);
app.commandLine.appendSwitch('disable-blink-features', 'AutomationControlled');
app.commandLine.appendSwitch('disable-features', 'IsolateOrigins,site-per-process');
app.commandLine.appendArgument('--no-sandbox');
const path = require('path');
const fs = require('fs');
const crypto = require('crypto');
const https = require('https');
const { spawn } = require('child_process');
const os = require('os');

process.on('uncaughtException', (err) => { console.error('Uncaught:', err); });
process.on('unhandledRejection', (reason) => { console.error('Unhandled rejection:', reason); });

let _mutexProc = null;

function isMultiInstanceEnabled() {
  return !!(loadSettings().multiInstance);
}

function startMutexHolder() {
  if (_mutexProc) return;
  const psScript = app.isPackaged
    ? path.join(process.resourcesPath, 'mutex.ps1')
    : path.join(__dirname, 'mutex.ps1');
  try {
    _mutexProc = spawn('powershell.exe', [
      '-NoProfile', '-NonInteractive', '-WindowStyle', 'Hidden',
      '-ExecutionPolicy', 'Bypass',
      '-File', psScript,
    ], {
      stdio: ['pipe', 'pipe', 'ignore'],
      windowsHide: true,
    });
    _mutexProc.on('exit', () => { _mutexProc = null; });
    _mutexProc.on('error', () => { _mutexProc = null; });
  } catch (e) {
    _mutexProc = null;
  }
}

function stopMutexHolder() {
  if (!_mutexProc) return;
  try { _mutexProc.kill(); } catch {}
  _mutexProc = null;
}

const settingsPath = path.join(app.getPath('userData'), 'settings.json');
function loadSettings() {
  try { if (!fs.existsSync(settingsPath)) return {}; return JSON.parse(fs.readFileSync(settingsPath, 'utf8')); } catch { return {}; }
}
function saveSettings(s) { fs.writeFileSync(settingsPath, JSON.stringify(s, null, 2), { mode: 0o600 }); }

const SALT = 'multiroblox-v1-salt-2025';
const ITERATIONS = 210_000;
const KEY_LEN = 32;
const DIGEST = 'sha512';

function getOrCreateDeviceKey() {
  const s = loadSettings();
  if (s._deviceKey && s._deviceKey.length === 64) {
    return Buffer.from(s._deviceKey, 'hex');
  }
  const key = crypto.randomBytes(KEY_LEN);
  saveSettings({ ...s, _deviceKey: key.toString('hex') });
  return key;
}

function deriveCustomKey(p) { return crypto.pbkdf2Sync(p, SALT, ITERATIONS, KEY_LEN, DIGEST); }

let _cachedKey = null;
function getEncryptionKey() {
  if (_cachedKey) return _cachedKey;
  const s = loadSettings();
  _cachedKey = (s.customKey && s.customKey.trim()) ? deriveCustomKey(s.customKey.trim()) : getOrCreateDeviceKey();
  return _cachedKey;
}
function invalidateKeyCache() { _cachedKey = null; }
function getEncryptionType() { return loadSettings().encryptionType || 'aes-256-gcm'; }

function encryptGCM(p, k) {
  const iv = crypto.randomBytes(12), c = crypto.createCipheriv('aes-256-gcm', k, iv);
  const enc = Buffer.concat([c.update(p, 'utf8'), c.final()]);
  return 'gcm:' + [iv.toString('base64'), c.getAuthTag().toString('base64'), enc.toString('base64')].join(':');
}
function decryptGCM(ct, k) {
  const s = ct.replace(/^gcm:/, '').split(':'); if (s.length < 3) return null;
  const iv = Buffer.from(s[0], 'base64'), tag = Buffer.from(s[1], 'base64'), data = Buffer.from(s[2], 'base64');
  const d = crypto.createDecipheriv('aes-256-gcm', k, iv); d.setAuthTag(tag);
  return d.update(data, undefined, 'utf8') + d.final('utf8');
}

function encryptCBC(p, k) {
  const iv = crypto.randomBytes(16), c = crypto.createCipheriv('aes-256-cbc', k, iv);
  const enc = Buffer.concat([c.update(p, 'utf8'), c.final()]);
  return 'cbc:' + [iv.toString('base64'), enc.toString('base64')].join(':');
}
function decryptCBC(ct, k) {
  const s = ct.replace(/^cbc:/, '').split(':'); if (s.length < 2) return null;
  const iv = Buffer.from(s[0], 'base64'), data = Buffer.from(s[1], 'base64');
  const d = crypto.createDecipheriv('aes-256-cbc', k, iv);
  return d.update(data, undefined, 'utf8') + d.final('utf8');
}

function encryptField(p) {
  const k = getEncryptionKey(), t = getEncryptionType();
  return t === 'aes-256-cbc' ? encryptCBC(p, k) : encryptGCM(p, k);
}
function decryptField(ct) {
  try {
    if (!ct) return null;
    const k = getEncryptionKey();
    if (ct.startsWith('cbc:')) return decryptCBC(ct, k);
    if (ct.startsWith('gcm:')) return decryptGCM(ct, k);
    return ct;
  } catch { return null; }
}

function encryptAccount(a) {
  const o = { ...a };
  if (o.cookie && !o.cookie.startsWith('gcm:') && !o.cookie.startsWith('cbc:')) {
    o.cookie = encryptField(o.cookie);
  }
  o._enc = true;
  return o;
}
function decryptAccount(a) {
  const o = { ...a };
  if (o.cookie) o.cookie = decryptField(o.cookie) ?? '';
  return o;
}

const dataPath = path.join(app.getPath('userData'), 'accounts.json');
function loadAccounts() {
  try { if (!fs.existsSync(dataPath)) return []; return JSON.parse(fs.readFileSync(dataPath, 'utf8')).map(decryptAccount); } catch { return []; }
}
function saveAccounts(a) { fs.writeFileSync(dataPath, JSON.stringify(a.map(encryptAccount), null, 2), { mode: 0o600 }); }

let win;
function createWindow() {
  win = new BrowserWindow({
    width: 780, height: 520, minWidth: 780, minHeight: 520,
    frame: false, backgroundColor: '#0e0e10',
    icon: path.join(__dirname, 'icon.ico'),
    webPreferences: { preload: path.join(__dirname, 'preload.js'), contextIsolation: true, nodeIntegration: false, sandbox: false },
    show: false,
  });
  win.loadFile(path.join(__dirname, 'index.html'));
  win.once('ready-to-show', () => win.show());
}
app.whenReady().then(() => {
  if (process.platform === 'win32') app.setAppUserModelId('com.multiroblox.app');
  if (isMultiInstanceEnabled()) startMutexHolder();
  createWindow();
});
app.on('window-all-closed', () => { if (process.platform !== 'darwin') app.quit(); });
app.on('will-quit', () => stopMutexHolder());

ipcMain.on('window-minimize', () => win.minimize());
ipcMain.on('window-maximize', () => win.isMaximized() ? win.unmaximize() : win.maximize());
ipcMain.on('window-close', () => win.close());
ipcMain.on('open-external', (_, url) => shell.openExternal(url));

ipcMain.handle('settings:load', () => loadSettings());
ipcMain.handle('settings:save', (_, data) => {
  saveSettings({ ...loadSettings(), ...data });
  if ('customKey' in data || 'encryptionType' in data) invalidateKeyCache();
  if ('multiInstance' in data) {
    if (data.multiInstance) startMutexHolder();
    else stopMutexHolder();
  }
  return true;
});
ipcMain.handle('multiinstance:status', () => ({ enabled: isMultiInstanceEnabled(), active: !!_mutexProc }));
ipcMain.handle('accounts:reencrypt', (_, plain) => { try { saveAccounts(plain); return { ok: true }; } catch (e) { return { ok: false, error: e.message }; } });

ipcMain.handle('accounts:load', () => loadAccounts());
ipcMain.handle('accounts:add', (_, account) => {
  const accounts = loadAccounts();
  const a = { id: Date.now().toString(), ...account, createdAt: new Date().toISOString(), lastUsed: null };
  accounts.push(a); saveAccounts(accounts); return a;
});
ipcMain.handle('accounts:remove', (_, id) => { saveAccounts(loadAccounts().filter(a => a.id !== id)); return true; });
ipcMain.handle('accounts:update', (_, id, data) => {
  const accounts = loadAccounts(), idx = accounts.findIndex(a => a.id === id);
  if (idx !== -1) { accounts[idx] = { ...accounts[idx], ...data }; saveAccounts(accounts); return accounts[idx]; }
  return null;
});
ipcMain.handle('accounts:reorder', (_, ids) => {
  const accounts = loadAccounts();
  const reordered = ids.map(id => accounts.find(a => a.id === id)).filter(Boolean);
  const rest = accounts.filter(a => !ids.includes(a.id));
  saveAccounts([...reordered, ...rest]);
  return true;
});

function fetchUserInfo(cookie) {
  return new Promise((resolve) => {
    const req = net.request({ method: 'GET', url: 'https://users.roblox.com/v1/users/authenticated', useSessionCookies: false, headers: { 'Cookie': `.ROBLOSECURITY=${cookie}`, 'Accept': 'application/json' } });
    let body = '';
    req.on('response', res => { res.on('data', c => body += c); res.on('end', () => { try { const d = JSON.parse(body); if (d && d.id) resolve({ ok: true, username: d.name, userId: String(d.id) }); else resolve({ ok: false, reason: body.slice(0, 200) }); } catch { resolve({ ok: false, reason: 'parse error' }); } }); });
    req.on('error', e => resolve({ ok: false, reason: e.message }));
    req.end();
  });
}

function httpsGet(url) {
  return new Promise((resolve) => {
    const req = https.get(url, {
      headers: { 'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36' }
    }, res => {
      let data = '';
      res.on('data', c => data += c);
      res.on('end', () => resolve({ status: res.statusCode, body: data }));
    });
    req.on('error', e => resolve({ status: 0, body: '', error: e.message }));
    req.setTimeout(5000, () => { req.destroy(); resolve({ status: 0, body: '', error: 'timeout' }); });
  });
}

function httpsPost(hostname, urlPath, headers, body) {
  return new Promise((resolve) => {
    const bodyBuf = body ? Buffer.from(JSON.stringify(body)) : Buffer.alloc(0);
    const req = https.request({
      hostname, path: urlPath, method: 'POST',
      headers: {
        ...headers,
        'Content-Type': 'application/json',
        'Content-Length': bodyBuf.length,
        'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36',
        'Accept': 'application/json',
      }
    }, res => {
      let data = '';
      res.on('data', c => data += c);
      res.on('end', () => resolve({ status: res.statusCode, headers: res.headers, body: data }));
    });
    req.on('error', e => resolve({ status: 0, headers: {}, body: '', error: e.message }));
    if (bodyBuf.length) req.write(bodyBuf);
    req.end();
  });
}

const _csrfCache = new Map();
const CSRF_TTL = 5 * 60_000; // 5 min — tokens stay valid much longer than 90s

const _ticketCache = new Map();
const TICKET_TTL     = 25_000;
const TICKET_MIN_GAP = 8_000;

// Serializing launch queue — prevents concurrent launches from all hammering
// auth.roblox.com at once and triggering 429s.
let _launchQueue = Promise.resolve();
let _lastLaunchTs = 0;
const LAUNCH_STAGGER = 4_000; // 4s between launches

async function getCSRFToken(cookie) {
  const cached = _csrfCache.get(cookie);
  if (cached && Date.now() - cached.ts < CSRF_TTL) return cached.token;

  const cookieHeader = `.ROBLOSECURITY=${cookie}`;
  for (const endpoint of ['/v2/logout', '/v1/logout']) {
    try {
      const res = await httpsPost('auth.roblox.com', endpoint, { 'Cookie': cookieHeader }, null);
      const token = res.headers['x-csrf-token'];
      if (token) {
        _csrfCache.set(cookie, { token, ts: Date.now() });
        return token;
      }
    } catch {}
  }
  return null;
}

const sleep = ms => new Promise(r => setTimeout(r, ms));

async function getAuthTicket(cookie, csrfToken) {
  const now = Date.now();
  const cached = _ticketCache.get(cookie);

  if (cached && (now - cached.ts) < TICKET_TTL) {
    return { ok: true, ticket: cached.ticket };
  }

  if (cached && (now - cached.ts) < TICKET_MIN_GAP) {
    await sleep(TICKET_MIN_GAP - (now - cached.ts));
  }

  const baseHeaders = {
    'Cookie': `.ROBLOSECURITY=${cookie}`,
    'Referer': 'https://www.roblox.com',
    'Origin': 'https://www.roblox.com',
  };

  let token = csrfToken;
  const delays = [0, 2000, 5000];

  for (let attempt = 0; attempt < 3; attempt++) {
    if (delays[attempt] > 0) await sleep(delays[attempt]);

    const res = await httpsPost('auth.roblox.com', '/v1/authentication-ticket', {
      ...baseHeaders,
      'X-CSRF-TOKEN': token,
    }, null);

    const ticket = res.headers['rbx-authentication-ticket'];
    if (ticket) {
      _ticketCache.set(cookie, { ticket, ts: Date.now() });
      return { ok: true, ticket };
    }

    if (res.status === 429) {
      _csrfCache.delete(cookie);
      const retryAfter = parseInt(res.headers['retry-after'] || '8', 10);
      await sleep(retryAfter * 1000);
      token = await getCSRFToken(cookie);
      if (!token) return { ok: false, error: 'Rate limited and could not refresh token. Wait a moment and try again.' };
      continue;
    }

    if (res.status === 403) {
      _csrfCache.delete(cookie);
      token = await getCSRFToken(cookie);
      if (!token) return { ok: false, error: 'Authentication failed (403). Cookie may be expired.' };
      continue;
    }

    return { ok: false, error: `Auth ticket request failed (HTTP ${res.status}). Try again in a moment.` };
  }

  return { ok: false, error: 'Still rate limited after 3 attempts. Please wait 30 seconds and try again.' };
}

async function getRobloxVersion() {
  try {
    const r = await httpsGet('https://clientsettingscdn.roblox.com/v2/client-version/WindowsPlayer');
    if (r.status === 200) {
      const d = JSON.parse(r.body);
      if (d && d.clientVersionUpload) return d.clientVersionUpload;
      if (d && d.version) return d.version;
    }
  } catch {}
  return null;
}

ipcMain.handle('roblox:getVersion', async () => {
  try { return await getRobloxVersion(); } catch { return null; }
});

async function extractCookie(ses) {
  try {
    await ses.cookies.flushStore();
    const all = await ses.cookies.get({ domain: '.roblox.com' });
    return all.find(c => c.name === '.ROBLOSECURITY' && c.value && c.value.length > 100) || null;
  } catch { return null; }
}

ipcMain.handle('roblox:validateCookie', async (_, cookie) => {
  return await fetchUserInfo(cookie);
});

let puppeteerBrowserPath = null;

async function ensureChrome() {
  try {
    // Check common system Chrome install paths first
    const systemChromePaths = [
      'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe',
      'C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe',
      path.join(os.homedir(), 'AppData', 'Local', 'Google', 'Chrome', 'Application', 'chrome.exe'),
    ];
    for (const p of systemChromePaths) {
      if (fs.existsSync(p)) return p;
    }

    const pb = (() => { try { return require('@puppeteer/browsers'); } catch { return null; } })();
    if (!pb) return null;
    const { install, Browser, detectBrowserPlatform, getInstalledBrowsers } = pb;
    const browserDir = path.join(app.getPath('userData'), 'chrome-for-login');

    if (fs.existsSync(browserDir)) {
      const installed = await getInstalledBrowsers({ cacheDir: browserDir });
      const chrome = installed.find(b => b.browser === Browser.CHROME);
      if (chrome && fs.existsSync(chrome.executablePath)) {
        return chrome.executablePath;
      }
    }

    if (win && !win.isDestroyed()) {
      win.webContents.send('chrome:download-progress', { status: 'downloading', percent: 0 });
    }

    const platform = detectBrowserPlatform();

    const buildId = await new Promise((resolve, reject) => {
      const req = net.request('https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions.json');
      let body = '';
      req.on('response', res => {
        res.on('data', d => body += d);
        res.on('end', () => {
          try {
            const json = JSON.parse(body);
            resolve(json.channels.Stable.version);
          } catch (e) { reject(e); }
        });
      });
      req.on('error', reject);
      req.end();
    });

    const result = await install({
      browser: Browser.CHROME,
      buildId,
      cacheDir: browserDir,
      platform,
      downloadProgressCallback: (downloaded, total) => {
        if (win && !win.isDestroyed()) {
          win.webContents.send('chrome:download-progress', {
            status: 'downloading',
            percent: total > 0 ? Math.round((downloaded / total) * 100) : 0
          });
        }
      }
    });

    if (win && !win.isDestroyed()) {
      win.webContents.send('chrome:download-progress', { status: 'done' });
    }

    return result.executablePath;
  } catch (e) {
    console.error('ensureChrome error:', e.message);
    return null;
  }
}

ipcMain.handle('roblox:openLogin', async () => {
  const hasPuppeteer = (() => { try { require('puppeteer-core'); return true; } catch { return false; } })();
  if (!hasPuppeteer) {
    return { success: false, error: 'Browser login is not available in this build. Use "Paste Cookie" instead.' };
  }
  const chromePath = await ensureChrome();
  if (!chromePath) {
    return { success: false, error: 'Failed to download Chrome. Check your internet connection and try again.' };
  }
  return puppeteerLogin(chromePath);
});

async function puppeteerLogin(chromePath) {
  return new Promise(async (resolve) => {
    let browser = null;
    let resolved = false;
    const cleanup = async () => { if (browser) { try { await browser.close(); } catch (_) {} browser = null; } };

    try {
      const puppeteer = (() => { try { return require('puppeteer-core'); } catch { return null; } })();
      if (!puppeteer) { resolve({ success: false, error: 'puppeteer-core not available in this build.' }); return; }
      browser = await puppeteer.launch({
        executablePath: chromePath,
        headless: false,
        defaultViewport: null,
        args: ['--no-sandbox', '--disable-setuid-sandbox', '--disable-blink-features=AutomationControlled', '--window-size=530,700'],
        ignoreDefaultArgs: ['--enable-automation', '--enable-blink-features=IdleDetection'],
      });

      // Use the default page that Chrome opens — reuse it instead of opening a second one
      const defaultPages = await browser.pages();
      const page = defaultPages.length > 0 ? defaultPages[0] : await browser.newPage();

      await page.evaluateOnNewDocument(`
        Object.defineProperty(navigator,'webdriver',{get:()=>false});
        Object.defineProperty(navigator,'plugins',{get:()=>[{name:'Chrome PDF Plugin',filename:'internal-pdf-viewer'}]});
      `);

      await page.goto('https://www.roblox.com/login', { waitUntil: 'domcontentloaded', timeout: 30000 });

      const poll = setInterval(async () => {
        if (resolved) return;
        try {
          const client = await page.createCDPSession();
          const { cookies } = await client.send('Network.getAllCookies');
          await client.detach();
          const rbxCookie = cookies.find(ck => ck.name === '.ROBLOSECURITY' && ck.domain.includes('roblox.com') && ck.value && ck.value.length > 100);
          if (!rbxCookie) return;
          resolved = true; clearInterval(poll);
          await cleanup();
          const info = await fetchUserInfo(rbxCookie.value);
          if (!info.ok) { resolve({ success: false, error: info.reason || 'Could not verify account.' }); return; }
          resolve({ success: true, cookie: rbxCookie.value, username: info.username, userId: info.userId });
        } catch (_) {}
      }, 1500);

      browser.on('disconnected', () => { clearInterval(poll); if (!resolved) { resolved = true; resolve({ success: false, error: 'Login window closed' }); } });
      ipcMain.once('login:cancel', async () => { clearInterval(poll); if (!resolved) { resolved = true; await cleanup(); resolve({ success: false, error: 'Login window closed' }); } });
    } catch (e) {
      console.error('puppeteerLogin error:', e.message);
      await cleanup();
      if (!resolved) resolve({ success: false, error: 'Failed to launch Chrome: ' + e.message });
    }
  });
}


const genHistoryPath = path.join(app.getPath('userData'), 'genhistory.json');

ipcMain.handle('genhistory:read', () => {
  try {
    if (!fs.existsSync(genHistoryPath)) return [];
    return JSON.parse(fs.readFileSync(genHistoryPath, 'utf8'));
  } catch { return []; }
});

ipcMain.handle('genhistory:write', (_, list) => {
  try {
    fs.writeFileSync(genHistoryPath, JSON.stringify(list, null, 2), { mode: 0o600 });
    return true;
  } catch { return false; }
});

ipcMain.handle('genhistory:clear', () => {
  try {
    fs.writeFileSync(genHistoryPath, '[]', { mode: 0o600 });
    return true;
  } catch { return false; }
});

function getFFlagPath() {
  try {
    const versionsBase = path.join(os.homedir(), 'AppData', 'Local', 'Roblox', 'Versions');
    if (!fs.existsSync(versionsBase)) return null;
    const dirs = fs.readdirSync(versionsBase)
      .filter(d => d.startsWith('version-') && fs.existsSync(path.join(versionsBase, d, 'RobloxPlayerBeta.exe')))
      .sort().reverse();
    if (!dirs.length) return null;
    return path.join(versionsBase, dirs[0], 'ClientSettings', 'ClientAppSettings.json');
  } catch { return null; }
}

ipcMain.handle('fflag:read', () => {
  try {
    const p = getFFlagPath();
    if (!p || !fs.existsSync(p)) return {};
    return JSON.parse(fs.readFileSync(p, 'utf8'));
  } catch { return {}; }
});

ipcMain.handle('fflag:write', (_, flags) => {
  try {
    const p = getFFlagPath();
    if (!p) return false;
    fs.mkdirSync(path.dirname(p), { recursive: true });
    fs.writeFileSync(p, JSON.stringify(flags, null, 2), 'utf8');
    return true;
  } catch { return false; }
});

async function resolveShareLink(shareCode, cookie, csrfToken) {
  // Port of evanovar/RobloxAccountManager resolve_share_url:
  // POST to sharelinks/v1/resolve-link with {linkId, linkType}
  // On 403, grab fresh CSRF from response header and retry

  const makeRequest = (csrf) => new Promise((resolve) => {
    for (const payload of [
      JSON.stringify({ linkId: shareCode, linkType: 'Server' }),
      JSON.stringify({ code: shareCode, type: 'Server' }),
    ]) {
      // We try payloads sequentially below, so just store them
    }
    // Try first payload, fall back to second if needed
    const tryPayload = (payloadStr, csrfHeader, cb) => {
      const req = https.request({
        hostname: 'apis.roblox.com',
        path: '/sharelinks/v1/resolve-link',
        method: 'POST',
        headers: {
          'Cookie': `.ROBLOSECURITY=${cookie}`,
          'X-CSRF-TOKEN': csrfHeader || '',
          'Content-Type': 'application/json',
          'Content-Length': Buffer.byteLength(payloadStr),
          'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36',
        },
      }, res => {
        let body = '';
        res.on('data', c => body += c);
        res.on('end', () => {
          console.log('[resolveShareLink] status:', res.statusCode, 'body:', body.slice(0, 400));
          cb(res.statusCode, res.headers, body);
        });
      });
      req.on('error', e => cb(0, {}, ''));
      req.setTimeout(8000, () => { req.destroy(); cb(0, {}, ''); });
      req.write(payloadStr);
      req.end();
    };

    const payloads = [
      JSON.stringify({ linkId: shareCode, linkType: 'Server' }),
      JSON.stringify({ code: shareCode, type: 'Server' }),
    ];

    const tryNext = (i, currentCsrf) => {
      if (i >= payloads.length) return resolve({ ok: false });
      tryPayload(payloads[i], currentCsrf, (status, headers, body) => {
        if (status === 200) {
          const pidM = body.match(/"placeId"\s*:\s*(\d+)/);
          const lcM = body.match(/"(?:linkCode|privateServerLinkCode|accessCode|linkcode)"\s*:\s*"([A-Za-z0-9_\-]+)"/);
          if (pidM && lcM) {
            return resolve({ ok: true, placeId: pidM[1], linkCode: lcM[1] });
          }
        }
        if (status === 403 && headers['x-csrf-token']) {
          // Retry same payload with fresh CSRF from response
          tryPayload(payloads[i], headers['x-csrf-token'], (status2, headers2, body2) => {
            if (status2 === 200) {
              const pidM = body2.match(/"placeId"\s*:\s*(\d+)/);
              const lcM = body2.match(/"(?:linkCode|privateServerLinkCode|accessCode|linkcode)"\s*:\s*"([A-Za-z0-9_\-]+)"/);
              if (pidM && lcM) {
                return resolve({ ok: true, placeId: pidM[1], linkCode: lcM[1] });
              }
            }
            tryNext(i + 1, currentCsrf);
          });
        } else {
          tryNext(i + 1, currentCsrf);
        }
      });
    };

    tryNext(0, csrfToken || '');
  });

  const result = await makeRequest(csrfToken);
  if (!result.ok) {
    return { ok: false, error: 'Could not resolve share link. It may be expired or invalid.' };
  }

  return { ok: true, placeId: result.placeId, linkCode: result.linkCode };
}

async function followRedirect(url) {
  return new Promise((resolve) => {
    const req = net.request({ method: 'GET', url, redirect: 'manual', useSessionCookies: false });
    req.on('response', res => {
      const loc = res.headers['location'];
      resolve(loc || url);
    });
    req.on('error', () => resolve(url));
    req.end();
  });
}

// Resolves the accessCode for a private server linkCode using the sharelinks API.
// This is the correct method — linkCode != accessCode, they are different tokens.
async function getAccessCode(placeId, linkCode, cookie, csrfToken) {
  // Primary: sharelinks resolve API
  try {
    const bodyStr = JSON.stringify({ shareCode: linkCode, shareType: 'Server' });
    const req = net.request({
      method: 'POST',
      url: 'https://apis.roblox.com/sharelinks/v1/resolve',
      useSessionCookies: false,
      headers: {
        'Cookie': `.ROBLOSECURITY=${cookie}`,
        'X-CSRF-TOKEN': csrfToken || '',
        'Content-Type': 'application/json',
        'Content-Length': Buffer.byteLength(bodyStr),
        'Accept': 'application/json',
        'Origin': 'https://www.roblox.com',
        'Referer': 'https://www.roblox.com',
        'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36',
      },
    });
    const result = await new Promise((resolve) => {
      let body = '';
      req.on('response', res => {
        res.on('data', c => body += c);
        res.on('end', () => {
          try {
            const d = JSON.parse(body);
            const inv = d?.privateServerInviteData
              || d?.resolvedShareData?.privateServerInviteData
              || d?.experienceInviteData?.privateServerInviteData;
            if (inv && inv.accessCode) resolve(inv.accessCode);
            else resolve(null);
          } catch { resolve(null); }
        });
      });
      req.on('error', () => resolve(null));
      req.write(bodyStr);
      req.end();
    });
    if (result) return result;
  } catch {}

  // Fallback: redirect scrape
  return new Promise((resolve) => {
    const req = https.request({
      hostname: 'www.roblox.com',
      path: `/games/${placeId}?privateServerLinkCode=${linkCode}`,
      method: 'GET',
      headers: {
        'Cookie': `.ROBLOSECURITY=${cookie}`,
        'Referer': 'https://www.roblox.com',
        'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36',
      },
    }, res => {
      const loc = res.headers['location'] || '';
      const match = loc.match(/[?&]accessCode=([^&]+)/);
      resolve(match ? match[1] : null);
      res.resume();
    });
    req.on('error', () => resolve(null));
    req.setTimeout(5000, () => { req.destroy(); resolve(null); });
    req.end();
  });
}

ipcMain.handle('roblox:launch', async (_, accountId, cookie, target) => {
  const result = await (_launchQueue = _launchQueue.then(() => _doLaunch(accountId, cookie, target)));
  return result;
});

const _watchedAccounts = new Map();
const _missCounts = new Map(); // consecutive "not found" counts per account
const MISS_THRESHOLD = 4;      // require 4 consecutive misses (~20s) before declaring closed
const POLL_INTERVAL  = 5000;   // poll every 5s
const LAUNCH_DELAY   = 15000;  // wait 15s after launch before first poll (covers launcher->game gap)

function _watchRoblox(accountId) {
  if (_watchedAccounts.has(accountId)) return;
  _missCounts.set(accountId, 0);
  const timer = setTimeout(() => _pollRoblox(accountId), LAUNCH_DELAY);
  _watchedAccounts.set(accountId, timer);
}

function _pollRoblox(accountId) {
  const cmd = process.platform === 'win32'
    ? 'tasklist /FI "IMAGENAME eq RobloxPlayerBeta.exe" /NH'
    : 'pgrep -x RobloxPlayer';
  const proc = spawn(process.platform === 'win32' ? 'cmd' : 'sh',
    process.platform === 'win32' ? ['/c', cmd] : ['-c', cmd],
    { windowsHide: true });
  let out = '';
  proc.stdout.on('data', d => { out += d; });
  proc.on('close', () => {
    const running = out.toLowerCase().includes('roblox');
    if (!running) {
      const misses = (_missCounts.get(accountId) || 0) + 1;
      _missCounts.set(accountId, misses);
      if (misses >= MISS_THRESHOLD) {
        // Confirmed closed after MISS_THRESHOLD consecutive misses
        _watchedAccounts.delete(accountId);
        _missCounts.delete(accountId);
        if (win && !win.isDestroyed()) win.webContents.send('roblox:closed', accountId);
        return;
      }
    } else {
      _missCounts.set(accountId, 0); // reset on any successful detection
    }
    const timer = setTimeout(() => _pollRoblox(accountId), POLL_INTERVAL);
    _watchedAccounts.set(accountId, timer);
  });
}

async function _doLaunch(accountId, cookie, target) {
  try {
    // Enforce stagger between launches to avoid 429
    const sinceLastLaunch = Date.now() - _lastLaunchTs;
    if (_lastLaunchTs > 0 && sinceLastLaunch < LAUNCH_STAGGER) {
      await sleep(LAUNCH_STAGGER - sinceLastLaunch);
    }
    const csrfToken = await getCSRFToken(cookie);
    if (!csrfToken) return { success: false, error: 'Failed to get CSRF token. Is the account cookie still valid?' };

    const ticketResult = await getAuthTicket(cookie, csrfToken);
    if (!ticketResult.ok) return { success: false, error: `Failed to get auth ticket: ${ticketResult.error}` };
    const { ticket } = ticketResult;

    const t = (target || '').trim();
    let launcherUrl = '';

    if (t) {
      if (/^\d+$/.test(t)) {
        launcherUrl = `https://assetgame.roblox.com/game/placelauncher.ashx?request=RequestGame&placeId=${t}&isPlayTogetherGame=false`;
      } else {
        let rawUrl = t.startsWith('http') ? t : 'https://' + t;

        try {
          const parsed0 = new URL(rawUrl);
          if (parsed0.hostname === 'ro.blox.com' || parsed0.hostname.endsWith('.ro.blox.com')) {
            rawUrl = await followRedirect(rawUrl);
          }
        } catch {}

        let parsedUrl;
        try { parsedUrl = new URL(rawUrl); } catch {}

        if (parsedUrl) {
          const privateCode = parsedUrl.searchParams.get('privateServerLinkCode');
          const shareCode = parsedUrl.searchParams.get('code');
          const shareType = parsedUrl.searchParams.get('type');
          const placeId = parsedUrl.pathname.match(/\/games\/(\d+)/)?.[1]
            || parsedUrl.pathname.match(/\/(\d+)/)?.[1];

          if (privateCode && placeId) {
            const accessCode = await getAccessCode(placeId, privateCode, cookie, csrfToken);
            if (!accessCode) return { success: false, error: 'Could not resolve private server access code. The link may be expired or you may not have permission.' };
            launcherUrl = `https://assetgame.roblox.com/game/PlaceLauncher.ashx?request=RequestPrivateGame&placeId=${placeId}&accessCode=${accessCode}&linkCode=${privateCode}`;

          } else if (parsedUrl.pathname === '/share' || (shareCode && shareType)) {
            const code = shareCode;
            if (!code) return { success: false, error: 'Invalid share link — no code found.' };
            // Resolve the share link to get placeId + accessCode so we can
            // launch via the auth-ticket launcher (same as every other path).
            // Opening a bare roblox://navigation/share_links URI bypasses the
            // auth ticket and lets Roblox use whatever account is logged in on
            // the system — which is the wrong account.
            const resolved = await resolveShareLink(code, cookie, csrfToken);
            console.log('[launch] resolveShareLink result:', JSON.stringify(resolved));
            if (!resolved.ok) return { success: false, error: resolved.error || 'Could not resolve share link. It may be expired or invalid.' };
            launcherUrl = `https://assetgame.roblox.com/game/PlaceLauncher.ashx?request=RequestGameJob&placeId=${resolved.placeId}&isPlayTogetherGame=false&linkCode=${resolved.linkCode}`;
            console.log('[launch] launcherUrl:', launcherUrl);

          } else if (placeId) {
            launcherUrl = `https://assetgame.roblox.com/game/placelauncher.ashx?request=RequestGame&placeId=${placeId}&isPlayTogetherGame=false`;

          } else {
            return { success: false, error: 'Could not find a Place ID in the URL.' };
          }
        } else {
          return { success: false, error: 'Unrecognised input. Enter a place ID, game URL, or private server link.' };
        }
      }
    }

    const launchTime = Date.now();
    const browserId = String(Math.floor(Math.random() * 9e12 + 1e12));
    let robloxUri;
    if (launcherUrl) {
      robloxUri = `roblox-player:1+launchmode:play+gameinfo:${ticket}+launchtime:${launchTime}+placelauncherurl:${encodeURIComponent(launcherUrl)}+browsertrackerid:${browserId}+robloxLocale:en_us+gameLocale:en_us+channel:+LaunchExp:InApp`;
    } else {
      robloxUri = `roblox-player:1+launchmode:app+gameinfo:${ticket}+launchtime:${launchTime}+browsertrackerid:${browserId}+robloxLocale:en_us+gameLocale:en_us`;
    }

    await shell.openExternal(robloxUri);

    _lastLaunchTs = Date.now();
    _ticketCache.delete(cookie);

    const accounts = loadAccounts();
    const idx = accounts.findIndex(a => a.id === accountId);
    if (idx !== -1) { accounts[idx].lastUsed = new Date().toISOString(); saveAccounts(accounts); }

    _watchRoblox(accountId);
    return { success: true };
  } catch (err) {
    return { success: false, error: err.message };
  }
}
