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
  ensureGenAccounts();
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
    const req = net.request({ method: 'GET', url: 'https://users.roblox.com/v1/users/authenticated', headers: { 'Cookie': `.ROBLOSECURITY=${cookie}`, 'Accept': 'application/json' } });
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
const CSRF_TTL = 90_000;

const _ticketCache = new Map();
const TICKET_TTL     = 25_000;
const TICKET_MIN_GAP = 8_000;

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

function getGenAccountsPath() {
  return path.join(app.getPath('userData'), 'genaccounts.json');
}

function ensureGenAccounts() {
  const dest = getGenAccountsPath();
  if (!fs.existsSync(dest)) {
    const bundled = path.join(process.resourcesPath, 'genaccounts.json');
    if (fs.existsSync(bundled)) {
      try { fs.copyFileSync(bundled, dest); return; } catch {}
    }
    const defaultData = [{ username: 'exampleuser', password: 'examplepassword' }];
    fs.writeFileSync(dest, JSON.stringify(defaultData, null, 2), { mode: 0o600 });
  }
}

ipcMain.handle('genaccounts:read', () => {
  try {
    ensureGenAccounts();
    const list = JSON.parse(fs.readFileSync(getGenAccountsPath(), 'utf8'));
    return list.map(entry => {
      if (typeof entry === 'string') return entry;
      if (entry.username && entry.password) return entry.username + ':' + entry.password;
      return null;
    }).filter(Boolean);
  } catch { return []; }
});

ipcMain.handle('genaccounts:write', (_, list) => {
  try {
    fs.writeFileSync(getGenAccountsPath(), JSON.stringify(list, null, 2), { mode: 0o600 });
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
  for (const shareType of ['Server', 'ExperienceInvite', 'ExperienceDetails']) {
    try {
      const bodyStr = JSON.stringify({ shareCode, shareType });
      const req = net.request({
        method: 'POST',
        url: 'https://apis.roblox.com/sharelinks/v1/resolve',
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
              if (inv && inv.placeId) {
                resolve({
                  ok: true,
                  placeId: String(inv.placeId),
                  accessCode: inv.accessCode || inv.linkCode || shareCode,
                  linkCode: inv.linkCode || shareCode,
                });
              } else {
                resolve({ ok: false, status: res.statusCode, body: body.slice(0, 300) });
              }
            } catch (e) { resolve({ ok: false, error: e.message }); }
          });
        });
        req.on('error', e => resolve({ ok: false, error: e.message }));
        req.write(bodyStr);
        req.end();
      });
      if (result.ok) return result;
    } catch {}
  }
  return { ok: false, error: 'Could not resolve share link. It may be expired or invalid.' };
}

async function followRedirect(url) {
  return new Promise((resolve) => {
    const req = net.request({ method: 'GET', url, redirect: 'manual' });
    req.on('response', res => {
      const loc = res.headers['location'];
      resolve(loc || url);
    });
    req.on('error', () => resolve(url));
    req.end();
  });
}

ipcMain.handle('roblox:launch', async (_, accountId, cookie, target) => {
  try {
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
            const linkCode = privateCode;
            launcherUrl = `https://assetgame.roblox.com/game/PlaceLauncher.ashx?request=RequestPrivateGame&placeId=${placeId}&linkCode=${linkCode}`;

          } else if (parsedUrl.pathname === '/share' || (shareCode && shareType)) {
            const code = shareCode;
            if (!code) return { success: false, error: 'Invalid share link — no code found.' };
            const linkType = shareType || 'Server';
            const nativeUri = `roblox://navigation/share_links?code=${code}&type=${linkType}`;
            await shell.openExternal(nativeUri);
            _ticketCache.delete(cookie);
            const accts = loadAccounts();
            const aidx = accts.findIndex(a => a.id === accountId);
            if (aidx !== -1) { accts[aidx].lastUsed = new Date().toISOString(); saveAccounts(accts); }
            return { success: true };

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

    _ticketCache.delete(cookie);

    const accounts = loadAccounts();
    const idx = accounts.findIndex(a => a.id === accountId);
    if (idx !== -1) { accounts[idx].lastUsed = new Date().toISOString(); saveAccounts(accounts); }
    return { success: true };
  } catch (err) {
    return { success: false, error: err.message };
  }
});
