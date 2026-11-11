let accounts = [], editAcc = null, toastTimer;
let packages = [], editingPackageId = null;
const _launchedIds = new Set();

// ── Logs ─────────────────────────────────────────────────────────────────────
const _logs = [];
const MAX_LOGS = 2000;
const LOG_CATS = { launch:'launch', crash:'crash', kill:'kill', cookie:'cookie', afk:'afk', enc:'enc', system:'system', close:'close' };

function logEntry(level, category, message, meta) {
  const entry = { ts: Date.now(), level, category, message, meta: meta || {} };
  _logs.push(entry);
  if (_logs.length > MAX_LOGS) _logs.shift(); // keep the most-recent tail
  if (document.getElementById('page-logs')?.classList.contains('active')) _appendLogRow(entry);
}

// Surface uncaught invoke() rejections in the Logs page instead of losing them silently.
window.addEventListener('unhandledrejection', (e) => {
  const msg = (e.reason && (e.reason.message || e.reason)) || 'Unknown error';
  logEntry('err', 'system', `Unhandled error: ${msg}`);
});

// Enter/Space activates role="button" elements for keyboard/AT users.
document.addEventListener('keydown', (e) => {
  if (e.key !== 'Enter' && e.key !== ' ') return;
  const el = e.target.closest('[role="button"]');
  if (!el) return;
  e.preventDefault();
  el.click();
});

function _logLine(e) {
  const t = new Date(e.ts);
  const ts = t.toLocaleTimeString('en-GB', { hour12:false }) + '.' + String(t.getMilliseconds()).padStart(3,'0');
  const cat = String(e.category || '').toUpperCase();
  const keys = Object.keys(e.meta || {}).filter(k => e.meta[k] !== null && e.meta[k] !== undefined);
  const meta = keys.length ? ' <span class="lg-meta">' + keys.map(k => `${esc(k)}=${esc(e.meta[k])}`).join(' ') + '</span>' : '';
  return `<div class="log-row log-row-${esc(e.level)}"><span class="lg-ts">${esc(ts)}</span><span class="lg-cat">${esc(cat)}</span><span class="lg-msg">${esc(e.message)}${meta}</span></div>`;
}

let _logFilter = 'all';
function _logVisible(e) {
  if (_logFilter === 'all') return true;
  if (_logFilter === 'err') return e.level === 'err' || e.category === 'crash';
  return e.category === _logFilter;
}

function setLogFilter(f) {
  _logFilter = f;
  document.querySelectorAll('#log-filter button').forEach(b => b.classList.toggle('active', b.dataset.f === f));
  syncSeg(document.getElementById('log-filter'));
  renderLogs();
  const el = document.getElementById('logs-list');
  if (el) el.scrollTop = el.scrollHeight;
}

function logsCopy() {
  const rows = _logs.filter(_logVisible);
  if (!rows.length) { toast('Nothing to copy', 'err'); return; }
  const text = rows.map(e => {
    const time = new Date(e.ts).toLocaleTimeString('en-GB', { hour12: false });
    const meta = Object.keys(e.meta || {}).filter(k => e.meta[k] !== null && e.meta[k] !== undefined).map(k => `${k}=${e.meta[k]}`).join(' ');
    return `${time} ${String(e.category || '').toUpperCase()} ${e.message}${meta ? ' ' + meta : ''}`;
  }).join('\n');
  navigator.clipboard.writeText(text).then(() => toast(`Copied ${rows.length} line${rows.length !== 1 ? 's' : ''}`, 'ok')).catch(() => toast('Could not copy the log', 'err'));
}

function logsClear() {
  _logs.length = 0;
  renderLogs();
}

function renderLogs() {
  const el = document.getElementById('logs-list');
  if (!el) return;
  if (!_logs.length) { el.innerHTML = '<div class="logs-empty"><span class="material-icons-round">terminal</span>No log entries yet.<span class="logs-empty-sub">Launches, closes and errors appear here as they happen.</span></div>'; return; }
  const rows = _logs.filter(_logVisible);
  if (!rows.length) { el.innerHTML = `<div class="logs-empty"><span class="material-icons-round">filter_list_off</span>Nothing matches this filter.<button class="btn btn-ghost" onclick="setLogFilter('all')">Show all</button></div>`; return; }
  // Tail behaviour: only auto-scroll to the newest line if already near the end.
  const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
  // The newest lines first; earlier ones load as you scroll up to them.
  el.innerHTML = rows.slice(-LOG_PAGE).map(_logLine).join('');
  if (atBottom) el.scrollTop = el.scrollHeight;
}

// Adds the next batch of earlier lines above the ones showing (all of them with
// `all`), keeping the lines you're looking at where they are.
const LOG_PAGE = 200;
function _loadEarlierLogs(all) {
  const el = document.getElementById('logs-list');
  if (!el) return;
  const rows = _logs.filter(_logVisible);
  const hidden = rows.length - el.querySelectorAll('.log-row').length;
  if (hidden <= 0) return;
  const before = el.scrollHeight;
  el.insertAdjacentHTML('afterbegin', rows.slice(all ? 0 : Math.max(0, hidden - LOG_PAGE), hidden).map(_logLine).join(''));
  el.scrollTop += el.scrollHeight - before;
}
document.getElementById('logs-list')?.addEventListener('scroll', e => { if (e.target.scrollTop < 120) _loadEarlierLogs(); }, { passive: true });

// Appends one row instead of rejoining/replacing the whole log list.
function _appendLogRow(entry) {
  const el = document.getElementById('logs-list');
  if (!el || !_logVisible(entry)) return;
  if (el.querySelector('.logs-empty')) el.innerHTML = '';
  const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
  el.insertAdjacentHTML('beforeend', _logLine(entry));
  while (el.children.length > MAX_LOGS) el.removeChild(el.firstChild);
  if (atBottom) el.scrollTop = el.scrollHeight;
}

// Native-style find (Ctrl+F) over the rendered log text. Uses window.find so
// selection, scroll-to-match and Ctrl+A/Ctrl+C all behave like a normal viewer.
function openLogFind() {
  const bar = document.getElementById('log-find');
  const inp = document.getElementById('log-find-input');
  if (!bar || !inp) return;
  bar.style.display = 'flex';
  _loadEarlierLogs(true); // find searches what's on the page, so put every line there
  inp.focus(); inp.select();
}
function closeLogFind() {
  const bar = document.getElementById('log-find');
  if (bar) bar.style.display = 'none';
  const sel = window.getSelection && window.getSelection();
  if (sel) sel.removeAllRanges();
  const c = document.getElementById('log-find-count');
  if (c) c.textContent = '';
}
function logFind(backwards) {
  const inp = document.getElementById('log-find-input');
  const c = document.getElementById('log-find-count');
  if (!inp) return;
  const q = inp.value;
  if (!q) { if (c) c.textContent = ''; return; }
  const found = window.find(q, false, !!backwards, true, false, false, false);
  if (c) c.textContent = found ? '' : 'No matches';
}
const _avatarCache = {};
let settings = {};

let _encMode = null;
function showEncModal(mode) {
  _encMode = mode;
  const title = document.getElementById('enc-title');
  const desc = document.getElementById('enc-desc');
  const action = document.getElementById('enc-action');
  const skip = document.getElementById('enc-skip');
  const err = document.getElementById('enc-err');
  const inp = document.getElementById('enc-input');
  if (err) err.style.display = 'none';
  if (inp) inp.value = '';
  if (action) action.disabled = false;
  if (mode === 'setup') {
    title.textContent = 'Create an encryption key';
    desc.textContent = 'Set an encryption key to protect your saved accounts. You will be asked for it every time you open the app.';
    action.textContent = 'Set key';
    if (skip) if (skip) skip.style.display = 'none';
  } else {
    title.textContent = 'Enter your encryption key';
    desc.textContent = 'Enter the key you set to unlock your saved accounts.';
    action.textContent = 'Unlock';
    if (skip) skip.style.display = 'none';
  }
  openModal('m-enc');
  setTimeout(() => inp && inp.focus(), 60);
}
function _encErr(m) { const e = document.getElementById('enc-err'); if (e) { e.textContent = m; e.style.display = 'block'; } }
async function submitEnc() {
  const inp = document.getElementById('enc-input');
  const action = document.getElementById('enc-action');
  const val = inp ? inp.value : '';
  // The input's Enter handler calls this directly, bypassing the disabled
  // button; a second Enter mid-unlock would otherwise run continueInit twice.
  if (action && action.disabled) return;
  if (action) action.disabled = true;
  try {
    if (_encMode === 'setup') {
      // trim(): the backend trims too, and a blank key there means "no
      // passphrase", so a whitespace-only entry would silently set none.
      if (!val.trim()) { _encErr('Please enter an encryption key.'); action.disabled = false; return; }
      const r = await api.encSetKey(val);
      if (!r || !r.ok) {
        const reason = r && r.error === 'decrypt failed'
          ? 'One or more saved accounts cannot be decrypted (their cookies may be from a different PC or Windows profile). Remove those accounts and try again.'
          : (r && r.error ? 'Could not set encryption key: ' + r.error : 'Could not set encryption key. Please try again.');
        _encErr(reason);
        action.disabled = false;
        return;
      }
    } else {
      if (!val) { _encErr('Please enter your encryption key.'); action.disabled = false; return; }
      const r = await api.encUnlock(val);
      if (!r || !r.ok) { _encErr('Wrong key. Please try again.'); logEntry('warn', 'enc', 'Failed encryption unlock attempt (wrong key)'); action.disabled = false; inp.value = ''; inp.focus(); return; }
      logEntry('ok', 'enc', 'Encryption key accepted, accounts unlocked');
    }
    closeModal('m-enc');
    await continueInit();
  } catch (e) { _encErr('Something went wrong. Please try again.'); if (action) action.disabled = false; }
}
async function skipEnc() {
  const skip = document.getElementById('enc-skip');
  if (skip) skip.disabled = true;
  try { await api.encSetKey(''); } catch {}
  closeModal('m-enc');
  await continueInit();
}

function clearAppData() {
  confirmAction('Delete ALL saved accounts, settings and this encryption key? This cannot be undone.', async () => {
    const btn = document.getElementById('enc-clear-data');
    if (btn) { btn.disabled = true; btn.textContent = 'Clearing...'; }
    let res;
    try { res = await api.clearAppData(); } catch (e) { res = { ok: false, error: e?.message || String(e) }; }
    if (res && res.ok) {
      // Full reload, not just re-running init(): the Rust side's own
      // in-memory caches get reset by the backend, but this is the simplest
      // way to guarantee every bit of frontend state (accounts, settings,
      // caches) starts completely fresh against the now-empty storage too.
      location.reload();
    } else {
      if (btn) { btn.disabled = false; btn.textContent = 'Clear data'; }
      _encErr(res && res.error ? 'Could not clear data: ' + res.error : 'Could not clear data. Please try again.');
    }
  });
}

async function init() {
  // Encryption gate: in passphrase mode the cookies can't be read until the key is
  // entered for this boot session. Show the popup first; only load data once ready.
  try {
    const st = await api.encStatus();
    if (st && st.mode === 'locked') { showEncModal('locked'); return; }
    if (st && st.mode === 'setup') {
      // enc_status says 'setup' whenever no passphrase is set, which includes
      // users who already chose to run without one; encSetupDone is what
      // records that choice, so only a genuinely fresh install gets prompted.
      let s = null;
      try { s = await api.loadSettings(); } catch {}
      if (!(s && s.encSetupDone)) { showEncModal('setup'); return; }
    }
  } catch {}
  await continueInit();
}

// Decrypt failure is itself proof a cookie is unusable; flag it instantly
// (no network round-trip needed) instead of waiting on recheckAllCookies.
function flagInvalidCookies(list) {
  for (const a of list) {
    if (a._cookieInvalid) {
      _cookieStatus[a.id] = 'dead';
      logEntry('warn', 'cookie', `Cookie could not be decrypted for ${a.username || a.id} (corrupted or wrong key)`, { accountId: a.id, username: a.username || null, userId: a.userId || null });
    }
  }
}

// Everything below registers backend listeners and starts loops, so it must
// only ever run once per page load, however the unlock flow reaches it.
let _initStarted = false;
async function continueInit() {
  if (_initStarted) return;
  _initStarted = true;
  [accounts, settings, packages] = await Promise.all([api.loadAccounts(), api.loadSettings(), api.loadPackages()]);
  logEntry('info', 'system', `Loaded ${accounts.length} account${accounts.length === 1 ? '' : 's'} from storage`);
  flagInvalidCookies(accounts);
  recheckAllCookies(true); // kick a full check off the moment cookies are readable, not on the 60s tick
  render();
  // put the toolbar back to the saved view + filter
  document.getElementById('vt-grid').classList.toggle('active', _acctView === 'grid');
  document.getElementById('vt-list').classList.toggle('active', _acctView === 'list');
  syncSeg(document.getElementById('acct-view'));
  document.querySelectorAll('#filter-menu button').forEach(b => b.classList.toggle('active', b.dataset.f === _acctFilter));
  document.getElementById('filter-btn').classList.toggle('on', _acctFilter !== 'all');
  renderPackages();
  applySettings();
  refreshMultiStatus();
  showAppVersion();
  startStatusPoll();
  if (settings.autoTrim) startAutoTrimLoop();
  startTrackingLoop();
  logEntry('info', 'system', 'MultiRoblox started', { version: 'v1', accounts: accounts.length, platform: navigator.platform });
  loadGenKey();
  try {
    const t = GEN_TYPES.find(x => x.value === localStorage.getItem('bloxgen_type'));
    if (t) genSetType(t.value, t.label, t.desc);
  } catch {}

  // Backend listeners go up before the awaits below: anything the backend
  // emitted while those round-trips were pending (a launch finishing, an
  // instance closing, a log line) used to be dropped on the floor.

  // Forward main-process log events into the renderer log
  api.onLogEntry(data => logEntry(data.level, data.category, data.message, data.meta));

  // main pushes the count off the watch tick; the local poll backs off below
  api.onRobloxCount(n => { _lastCountPushAt = Date.now(); _mixRunning = n; setRunningBadges(n); });

  // Fires the moment the process actually exists (see native.rs do_launch),
  // instead of waiting on the whole launch call: CSRF fetch, auth ticket,
  // inter-launch stagger, trailing bookkeeping; to resolve back to the
  // caller. markLaunched() is idempotent, so the startAccount() success path
  // below calling it again once its own await resolves is harmless.
  api.onRobloxStarted(id => markLaunched(id));

  api.onRobloxClosed(id => {
    _launchedIds.delete(id);
    syncRunTimes();
    const closedAcct = accounts.find(a => a.id === id);
    logEntry('info', 'close', `Roblox closed for ${closedAcct ? closedAcct.username : id}`, { accountId: id, username: closedAcct?.username || null, userId: closedAcct?.userId || null });
    const card = document.querySelector(`.card[data-id="${id}"]`);
    if (card) card.classList.remove('is-live');
    const dot = document.querySelector(`.card[data-id="${id}"] .card-dot`);
    if (dot) { dot.classList.remove('launched'); dot.title = 'Not launched'; }
    refreshPkgAvatarStatus();
    pollRunningCount();
  });

  api.onAllRobloxClosed(() => {
    logEntry('warn', 'close', 'All Roblox instances closed');
    _launchedIds.clear();
    document.querySelectorAll('.card.is-live').forEach(c => c.classList.remove('is-live'));
    document.querySelectorAll('.card-dot.launched').forEach(d => { d.classList.remove('launched'); d.title = 'Not launched'; });
    refreshPkgAvatarStatus();
    pollRunningCount();
    if (document.getElementById('page-mixer')?.classList.contains('active')) mixRefreshRunning();
  });

  // Chrome download progress
  api.onChromeProgress(data => {
    const dlDiv = document.getElementById('login-dl');
    const waitDiv = document.getElementById('login-waiting');
    if (!dlDiv || !waitDiv) return;
    if (data.status === 'downloading') {
      dlDiv.style.display = '';
      waitDiv.style.display = 'none';
      if (data.percent !== undefined) {
        document.getElementById('dl-bar').style.width = data.percent + '%';
        document.getElementById('dl-pct').textContent = data.percent + '%';
      }
    } else if (data.status === 'done') {
      dlDiv.style.display = 'none';
      waitDiv.style.display = '';
    }
  });

  try { const afkStat = await api.antiAfkStatus(); if (afkStat && afkStat.enabled) logEntry('info', 'afk', `Anti-AFK is enabled on startup (active: ${afkStat.active})`, { enabled: afkStat.enabled, active: afkStat.active }); } catch {}
  try { _genHistory = (await api.readGenHistory()) || []; genRenderHistory(); } catch {}
}
init();

// ── Theme ──────────────────────────────────────────────────────────────────
var THEMES = ['dark','light','mono','midnight','aurora','sunset','crimson','ocean','grape','forest','amber','rose','graphite'];
// Set by the inline bootstrap in index.html; the literal is only a fallback
// for the case where that script somehow didn't run.
var DEFAULT_THEME = window.DEFAULT_THEME || 'dark';
// Theme classes live on <html>, not <body>: index.html's inline bootstrap
// sets them before the document body is even parsed, which is the only way to
// have the palette in place for the first paint. Keep this in sync with that
// bootstrap's DEFAULT_THEME.
function applyTheme(name) {
  if (THEMES.indexOf(name) < 0) name = DEFAULT_THEME;
  const root = document.documentElement;
  root.classList.remove('light');
  THEMES.forEach(t => { if (t !== 'dark' && t !== 'light') root.classList.remove('theme-' + t); });
  if (name === 'light') root.classList.add('light');
  else if (name !== 'dark') root.classList.add('theme-' + name);
  document.querySelectorAll('.theme-card').forEach(c => c.classList.toggle('sel', c.dataset.theme === name));
}
function currentTheme() { try { return localStorage.getItem('ui-theme') || DEFAULT_THEME; } catch { return DEFAULT_THEME; } }
let _themeAnimTimer = null;
function setTheme(name) {
  if (THEMES.indexOf(name) < 0) name = DEFAULT_THEME;
  // Cross-fade the palette, but only when the theme actually changes and only
  // once the first paint is behind us: doing it on the startup call would
  // animate the whole window in from the stylesheet defaults, which is the
  // very flash the inline bootstrap exists to prevent.
  const root = document.documentElement;
  if (document.readyState !== 'loading' && name !== currentTheme()) {
    root.classList.add('theme-anim');
    clearTimeout(_themeAnimTimer);
    _themeAnimTimer = setTimeout(() => root.classList.remove('theme-anim'), 220);
  }
  applyTheme(name);
  try { localStorage.setItem('ui-theme', name); } catch {}
}
function openDiscord() {
  api.openExternal('https://discord.gg/kZ8MZZ8dTF');
}
// Re-runs what the inline bootstrap already did, so the theme-card selection
// state gets marked and the choice is persisted for anyone still on the old
// pre-'ui-theme' key. Applying the same class twice is a no-op.
(function() {
  let t;
  try {
    t = localStorage.getItem('ui-theme');
    if (!t) { const old = localStorage.getItem('theme'); t = (old === 'light') ? 'light' : DEFAULT_THEME; }
  } catch { t = DEFAULT_THEME; }
  setTheme(t || DEFAULT_THEME);
})();

// ── Generator API key persistence ──────────────────────────────────────────
// Encrypted by the backend like the cookies. It used to sit in localStorage in
// plain text, so a key found there is moved across once and then removed.
async function loadGenKey() {
  let key = '';
  try { key = (await api.loadGenKey()) || ''; } catch {}
  let legacy = null;
  try { legacy = localStorage.getItem('bloxgen_apikey'); } catch {}
  if (legacy !== null) {
    try {
      if (!key && legacy) { await api.saveGenKey(legacy); key = legacy; }
      localStorage.removeItem('bloxgen_apikey');
    } catch {} // not saved: leave the old copy for the next start to retry
  }
  const el = document.getElementById('gen-apikey');
  if (el && !el.value && typeof key === 'string') el.value = key;
  genOnKeyInput();
}
let _genKeySaveTimer = null;
function saveGenKeySoon() {
  clearTimeout(_genKeySaveTimer);
  _genKeySaveTimer = setTimeout(() => {
    const el = document.getElementById('gen-apikey');
    if (el) api.saveGenKey(el.value).catch(() => {});
  }, 400);
}


async function showAppVersion() {
  let v;
  try { v = await api.getAppVersion(); } catch { return; }
  const verEl = document.getElementById('stat-app-ver');
  if (verEl && v) verEl.textContent = 'Version ' + v;
}

function applySettings() {
  // Reports what the backend actually writes rather than offering a choice it
  // doesn't honour: scrypt-derived AES-256-GCM once a key is set, otherwise
  // Windows DPAPI tied to the logged-in account (see encryption.rs).
  const encAlgo = document.getElementById('stat-enc-algo');
  const encDesc = document.getElementById('stat-enc-algo-desc');
  if (encAlgo) encAlgo.textContent = settings.keySet ? 'AES-256-GCM' : 'Windows DPAPI';
  if (encDesc) {
    encDesc.textContent = settings.keySet
      ? 'Cookies are encrypted with your key (scrypt + AES-256-GCM).'
      : 'Cookies are encrypted with Windows DPAPI, tied to your Windows account. Set a key below to use a passphrase instead.';
  }
  const keyIn = document.getElementById('custom-key');
  if (keyIn) { keyIn.value = ''; keyIn.placeholder = settings.keySet ? 'Key is set, type to update it' : 'e.g. SecureKey1234@A#'; }
  const afk = document.getElementById('set-antiafk');
  if (afk) afk.checked = !!settings.antiAfk;
  const afkSb = document.getElementById('sb-antiafk');
  if (afkSb) afkSb.checked = !!settings.antiAfk;
  const afkIv = document.getElementById('set-antiafk-interval');
  if (afkIv) {
    const mins = Math.round(Math.min(settings.antiAfkInterval || 17 * 60, 17 * 60) / 60);
    afkIv.value = mins;
    updateSliderFill(afkIv);
    const afkIvVal = document.getElementById('set-antiafk-interval-val');
    if (afkIvVal) afkIvVal.textContent = mins + ' min';
  }
  paintAntiAfkKey();
  layoutCfgLoad();
  const pv = document.getElementById('set-preview');
  if (pv) pv.checked = !!settings.profilePreview;
  startPreviewPoll();
  const trim = document.getElementById('set-autotrim');
  if (trim) trim.checked = !!settings.autoTrim;
  const trimIv = document.getElementById('set-autotrim-interval');
  if (trimIv) {
    const mins = settings.autoTrimIntervalMin || 5;
    trimIv.value = mins;
    updateSliderFill(trimIv);
    const trimIvVal = document.getElementById('set-autotrim-interval-val');
    if (trimIvVal) trimIvVal.textContent = mins + ' min';
  }
  const ramIv = document.getElementById('set-ramlimit');
  if (ramIv) {
    ramIv.value = Number(settings.ramLimitMb) || 0;
    updateSliderFill(ramIv);
    const ramVal = document.getElementById('set-ramlimit-val');
    if (ramVal) ramVal.textContent = ramLimitLabel(settings.ramLimitMb);
  }
  const relaunch = document.getElementById('set-autorelaunch');
  if (relaunch) relaunch.checked = !!settings.autoRelaunch;
  const lowPriority = document.getElementById('set-lowpriority');
  if (lowPriority) lowPriority.checked = settings.lowPriorityMultiInstance === true;
  const protectSignIn = document.getElementById('set-protectsignin');
  if (protectSignIn) protectSignIn.checked = settings.protectSignIn !== false;
  // Off by default: quitting on close is the long-standing behaviour, and
  // silently vanishing into the tray instead is a surprise worth opting into.
  const hideTray = document.getElementById('set-hidetotray');
  if (hideTray) hideTray.checked = !!settings.hideToTray;
}

// Registered once from continueInit. It used to live in applySettings, which
// also runs after every encryption-key save, so each save stacked another
// listener and every progress event was handled several times over.
let _acctQuery = '', _acctFilter = (() => { try { const f = localStorage.getItem('mr-acct-filter'); return (f && f !== 'running' && f !== 'idle') ? f : 'all'; } catch { return 'all'; } })(), _acctView = (() => { try { return localStorage.getItem('mr-acct-view') === 'list' ? 'list' : 'grid'; } catch { return 'grid'; } })();
function visibleAccounts() {
  let list = [...accounts];
  if (_acctQuery) {
    const q = _acctQuery;
    list = list.filter(a => (a.nickname || a.username || '').toLowerCase().includes(q) || String(a.userId || '').includes(q) || String(a.tag || '').toLowerCase().includes(q));
  }
  if (_acctFilter === 'running') list = list.filter(a => _launchedIds.has(a.id));
  else if (_acctFilter === 'idle') list = list.filter(a => !_launchedIds.has(a.id));
  else if (_acctFilter === 'valid-first') list.sort((a, b) => {
    const s = id => _cookieStatus[id] === 'dead' ? 1 : 0;
    return s(a.id) - s(b.id);
  });
  else if (_acctFilter === 'invalid-first') list.sort((a, b) => {
    const s = id => _cookieStatus[id] === 'dead' ? 0 : 1;
    return s(a.id) - s(b.id);
  });
  return list;
}
let _searchTimer;
function onAcctSearch(v) {
  _acctQuery = (v || '').trim().toLowerCase();
  clearTimeout(_searchTimer);
  _searchTimer = setTimeout(render, 120); // debounce so a long list isn't rebuilt on every keystroke
}
function toggleFilterMenu(e) { if (e) e.stopPropagation(); document.getElementById('filter-menu').classList.toggle('open'); }
function setAcctFilter(f) {
  _acctFilter = f;
  try { localStorage.setItem('mr-acct-filter', (f === 'running' || f === 'idle') ? 'all' : f); } catch {}
  _gridAnimate = true;
  document.querySelectorAll('#filter-menu button').forEach(b => b.classList.toggle('active', b.dataset.f === f));
  document.getElementById('filter-menu').classList.remove('open');
  document.getElementById('filter-btn').classList.toggle('on', f !== 'all');
  render();
}
function setAcctView(v) {
  _acctView = v;
  try { localStorage.setItem('mr-acct-view', v); } catch {}
  _gridAnimate = true;
  document.getElementById('vt-grid').classList.toggle('active', v === 'grid');
  document.getElementById('vt-list').classList.toggle('active', v === 'list');
  syncSeg(document.getElementById('acct-view'));
  render();
}
document.addEventListener('click', e => {
  const fm = document.getElementById('filter-menu');
  if (fm && fm.classList.contains('open') && !e.target.closest('.filter-wrap')) fm.classList.remove('open');
});

function paintWelcome() {
  const on = !!document.getElementById('sb-antiafk')?.checked;
  const btn = document.getElementById('welcome-afk');
  if (!btn) return;
  btn.textContent = on ? 'Turned on' : 'Turn on';
  btn.disabled = on;
}
function welcomeAntiAfk() {
  document.getElementById('sb-antiafk').checked = true;
  toggleAntiAfk('sb');
}

function toggleAntiAfk(src) {
  const el = document.getElementById(src === 'sb' ? 'sb-antiafk' : 'set-antiafk');
  const on = el.checked;
  settings.antiAfk = on;
  api.saveSettings({ antiAfk: on });
  const a = document.getElementById('set-antiafk'); if (a) a.checked = on;
  const b = document.getElementById('sb-antiafk'); if (b) b.checked = on;
  toast(on ? 'Anti-AFK on, accounts stay connected' : 'Anti-AFK off', on ? 'ok' : 'err');
  paintWelcome();
}

// ── Anti-AFK key ──
// keyCode is the Windows virtual-key code the helper presses, as the page reports it.
const VK_NAMES = { 8: 'Backspace', 9: 'Tab', 13: 'Enter', 16: 'Shift', 17: 'Ctrl', 18: 'Alt', 19: 'Pause', 20: 'Caps Lock', 27: 'Esc', 32: 'Space', 33: 'Page Up', 34: 'Page Down', 35: 'End', 36: 'Home', 37: 'Left', 38: 'Up', 39: 'Right', 40: 'Down', 44: 'Print Screen', 45: 'Insert', 46: 'Delete', 144: 'Num Lock', 145: 'Scroll Lock', 186: ';', 187: '=', 188: ',', 189: '-', 190: '.', 191: '/', 192: '`', 219: '[', 220: '\\', 221: ']', 222: "'" };
function vkName(vk) {
  if (VK_NAMES[vk]) return VK_NAMES[vk];
  if ((vk >= 48 && vk <= 57) || (vk >= 65 && vk <= 90)) return String.fromCharCode(vk);
  if (vk >= 112 && vk <= 135) return 'F' + (vk - 111);
  if (vk >= 96 && vk <= 105) return 'Num ' + (vk - 96);
  return 'Key ' + vk;
}
function antiAfkKeyOf() { const k = parseInt(settings.antiAfkKey, 10); return k >= 1 && k <= 255 ? k : 16; }
function paintAntiAfkKey() {
  const btn = document.getElementById('set-antiafk-key');
  if (!btn) return;
  btn.textContent = vkName(antiAfkKeyOf());
  btn.classList.remove('listening');
  const reset = document.getElementById('set-antiafk-key-reset');
  if (reset) reset.style.visibility = antiAfkKeyOf() === 16 ? 'hidden' : '';
}
function setAntiAfkKey(vk) {
  settings.antiAfkKey = vk;
  api.saveSettings({ antiAfkKey: vk });
  paintAntiAfkKey();
  toast('Anti-AFK will press ' + vkName(vk === 0 ? 16 : vk), 'ok');
}
let _keyPicking = false;
function antiAfkPickKey() {
  const btn = document.getElementById('set-antiafk-key');
  if (!btn || _keyPicking) return;
  _keyPicking = true;
  btn.textContent = 'Press a key…';
  btn.classList.add('listening');
  const done = vk => {
    document.removeEventListener('keydown', onKey, true);
    document.removeEventListener('mousedown', onAway, true);
    _keyPicking = false;
    if (vk) setAntiAfkKey(vk); else paintAntiAfkKey();
  };
  const onKey = e => {
    e.preventDefault(); e.stopPropagation();
    if (e.key === 'Escape') { done(0); return; }
    const vk = e.keyCode;
    // The Windows and menu keys open things outside the game; 229 is an input method's.
    if (!vk || vk === 229 || vk === 91 || vk === 92 || vk === 93) { toast("That key can't be used", 'err'); return; }
    done(vk);
  };
  const onAway = e => { if (!e.target.closest('#set-antiafk-key')) done(0); };
  document.addEventListener('keydown', onKey, true);
  setTimeout(() => document.addEventListener('mousedown', onAway, true), 0);
}
function antiAfkResetKey() { setAntiAfkKey(0); }

// ── Game previews on running tiles ──
// A 160-pixel JPEG of each running window every 30 seconds, only while the app is in
// view, laid over the account's profile picture (which stays underneath).
const PREVIEW_MS = 30000;
let _previews = {}, _previewTimer = null, _previewBusy = false;
function startPreviewPoll() {
  clearInterval(_previewTimer);
  _previewTimer = null;
  if (!settings.profilePreview) { paintPreviews(); return; }
  pollPreviews();
  _previewTimer = setInterval(pollPreviews, PREVIEW_MS);
}
async function pollPreviews() {
  if (_previewBusy || !settings.profilePreview || _appAway || document.hidden || !_launchedIds.size) return;
  _previewBusy = true;
  try { Object.assign(_previews, (await api.windowPreviews()) || {}); paintPreviews(); }
  catch {} finally { _previewBusy = false; }
}
function paintPreviews() {
  document.querySelectorAll('#grid .card[data-id]').forEach(card => {
    const id = card.dataset.id, av = card.querySelector('.card-av');
    if (!av) return;
    if (!_launchedIds.has(id)) delete _previews[id];
    const url = settings.profilePreview ? _previews[id] : null;
    let img = av.querySelector(':scope > .av-live');
    if (!url) { if (img) img.remove(); return; }
    if (!img) { img = document.createElement('img'); img.className = 'av-live'; img.alt = ''; av.appendChild(img); }
    if (img._url !== url) { img._url = url; img.src = url; }
  });
}
function togglePreview() {
  const on = !!document.getElementById('set-preview').checked;
  settings.profilePreview = on;
  api.saveSettings({ profilePreview: on });
  startPreviewPoll();
  toast(on ? 'Game previews on, refreshed every 30 seconds' : 'Game previews off', on ? 'ok' : 'err');
}

// ── Auto layout ──
// The button opens a dialog to set the layout up, then lines the running windows up as
// asked; it reads "Layout on" while it's in place, and the dialog can put the windows back.
// Windows started while it's on join the rows.
const LAYOUT_SIZES = { tiny: [320, 180], small: [480, 270], medium: [640, 360], large: [960, 540] };
let _layoutCfg = { size: 'tiny', w: 320, h: 180, corner: 0, gap: 0 };
let _layoutOn = false, _layoutUsed = null, _relayoutTimer = null;
const _layoutPending = new Set(); // accounts that opened since the layout and haven't got a cell yet
function layoutCfgLoad() {
  const c = settings.layoutConfig;
  if (c && typeof c === 'object') _layoutCfg = { ..._layoutCfg, ...c };
}
function paintLayoutBtn() {
  const btn = document.getElementById('layout-btn');
  if (!btn) return;
  btn.setAttribute('aria-pressed', String(_layoutOn));
  btn.title = _layoutOn ? 'Windows are lined up. Open to change or undo it' : 'Line up your running windows on this screen';
  document.getElementById('layout-icon').textContent = _layoutOn ? 'grid_on' : 'grid_view';
  document.getElementById('layout-label').textContent = _layoutOn ? 'Layout on' : 'Auto layout';
}
function layPaint() {
  const c = _layoutCfg;
  document.querySelectorAll('#m-layout .lay-opt').forEach(b => {
    const v = b.dataset.v, cur = c[b.dataset.g];
    b.classList.toggle('active', String(cur) === v);
  });
  document.getElementById('lay-custom').classList.toggle('show', c.size === 'custom');
  const w = document.getElementById('lay-w'), h = document.getElementById('lay-h');
  if (document.activeElement !== w) w.value = c.w;
  if (document.activeElement !== h) h.value = c.h;
}
function layPick(group, value) {
  _layoutCfg[group] = value;
  if (group === 'size' && LAYOUT_SIZES[value]) [_layoutCfg.w, _layoutCfg.h] = LAYOUT_SIZES[value];
  layPaint();
  layFit();
}
function layCustom() {
  _layoutCfg.w = parseInt(document.getElementById('lay-w').value, 10) || 0;
  _layoutCfg.h = parseInt(document.getElementById('lay-h').value, 10) || 0;
  layFit();
}
// A rough count of how many windows fit, with a title bar and thin edges around each.
function layFit() {
  const el = document.getElementById('lay-fit');
  const c = _layoutCfg, k = devicePixelRatio || 1;
  const w = Math.min(1280, Math.max(160, c.w || 0)), h = Math.min(720, Math.max(90, c.h || 0));
  const cols = Math.max(1, Math.floor((screen.availWidth * k + c.gap) / (w + 2 + c.gap)));
  const rows = Math.max(1, Math.floor((screen.availHeight * k + c.gap) / (h + 32 + c.gap)));
  const chosen = document.querySelectorAll('#lay-accts input:checked').length;
  const fits = cols * rows;
  el.textContent = `About ${fits} window${fits === 1 ? '' : 's'} fit${fits === 1 ? 's' : ''} on this screen at this size.` +
    (chosen > fits ? ' The rest start over from the corner and sit on top of the first ones.' : '');
}
function openAutoLayout() {
  if (!_launchedIds.size && !_layoutOn) { toast('Start an account first, then line the windows up'); return; }
  layoutCfgLoad();
  document.getElementById('lay-accts').innerHTML = accounts.filter(a => _launchedIds.has(a.id)).map(a => `
    <label class="lay-acct"><span>${esc(a.nickname || a.username || 'Unknown')}</span>
      <span class="toggle sm"><input type="checkbox" value="${esc(a.id)}" checked onchange="layFit()"/><span class="toggle-trk"></span></span>
    </label>`).join('');
  document.getElementById('lay-undo').style.display = _layoutOn ? '' : 'none';
  document.getElementById('lay-go').textContent = _layoutOn ? 'Line up again' : 'Line up';
  layPaint();
  layFit();
  openModal('m-layout');
}
async function layoutApply() {
  const go = document.getElementById('lay-go');
  if (go.disabled) return;
  const ids = [...document.querySelectorAll('#lay-accts input:checked')].map(i => i.value);
  if (!ids.length) { toast('Pick at least one account'); return; }
  const c = _layoutCfg;
  if (!(c.w >= 160 && c.w <= 1280 && c.h >= 90 && c.h <= 720)) { toast('Size must be 160 × 90 up to 1280 × 720', 'err'); return; }
  // Every account ticked means "all running", so one that starts later is lined up too.
  const opts = { w: c.w, h: c.h, corner: Number(c.corner) || 0, gap: Number(c.gap) || 0, ids: ids.length === _launchedIds.size ? [] : ids };
  go.disabled = true;
  try {
    const r = await api.layoutWindows(true, opts);
    if (!r || !r.ok) { toast((r && r.error) || 'Could not line the windows up', 'err'); return; }
    if (!r.count) { toast('No windows to line up. Minimized ones are skipped', 'err'); return; }
    _layoutOn = true;
    // `skip`: the running accounts left out, which stay out when others join.
    _layoutUsed = { ...opts, skip: accounts.filter(a => _launchedIds.has(a.id) && !ids.includes(a.id)).map(a => a.id) };
    _layoutPending.clear();
    settings.layoutConfig = { size: c.size, w: c.w, h: c.h, corner: c.corner, gap: c.gap };
    api.saveSettings({ layoutConfig: settings.layoutConfig });
    paintLayoutBtn();
    closeModal('m-layout');
    const s = r.count === 1 ? '' : 's';
    toast(`Lined up ${r.count} window${s}`, 'ok');
    logEntry('info', 'system', `Auto layout: ${r.count} window${s} lined up at ${c.w} × ${c.h}`);
  } catch (e) { toast('Could not line the windows up: ' + (e?.message || e), 'err'); }
  finally { go.disabled = false; }
}
async function layoutUndo() {
  try {
    const r = await api.layoutWindows(false);
    if (!r || !r.ok) { toast((r && r.error) || 'Could not put the windows back', 'err'); return; }
    _layoutOn = false;
    _layoutUsed = null;
    _layoutPending.clear();
    paintLayoutBtn();
    closeModal('m-layout');
    const s = r.count === 1 ? '' : 's';
    toast(`Put ${r.count} window${s} back`, 'ok');
    logEntry('info', 'system', `Auto layout undone: ${r.count} window${s} put back`);
  } catch (e) { toast('Could not put the windows back: ' + (e?.message || e), 'err'); }
}
// A client that opens while the layout is on takes the next free cell by itself. Its window
// takes a while to appear, so this looks again every few seconds for about 40 seconds. The
// helper leaves windows that are already lined up where they are, even ones you moved.
function relayoutSoon(id) {
  if (!_layoutOn || !_layoutUsed || _layoutUsed.skip.includes(id) || _layoutPending.has(id)) return;
  _layoutPending.add(id);
  clearTimeout(_relayoutTimer);
  _relayoutTimer = setTimeout(() => relayoutTick(0), 3000);
}
async function relayoutTick(tries) {
  _relayoutTimer = null;
  for (const id of [..._layoutPending]) if (!_launchedIds.has(id)) _layoutPending.delete(id); // closed meanwhile
  if (!_layoutOn || !_layoutUsed || !_layoutPending.size) { _layoutPending.clear(); return; }
  const u = _layoutUsed;
  const ids = [..._launchedIds].filter(id => !u.skip.includes(id));
  let placed = 0;
  try {
    const r = await api.layoutWindows(true, { w: u.w, h: u.h, corner: u.corner, gap: u.gap, ids, append: true });
    if (r && r.ok) placed = r.count;
  } catch {}
  for (const id of [..._layoutPending].slice(0, placed)) _layoutPending.delete(id);
  if (placed) logEntry('info', 'system', `Auto layout: ${placed} new window${placed === 1 ? '' : 's'} lined up`);
  if (_layoutPending.size && tries < 10) _relayoutTimer = setTimeout(() => relayoutTick(tries + 1), 4000);
  else _layoutPending.clear();
}

function antiAfkIntervalInput(v) {
  document.getElementById('set-antiafk-interval-val').textContent = v + ' min';
  updateSliderFill(document.getElementById('set-antiafk-interval'));
}
function antiAfkIntervalCommit() {
  const mins = parseInt(document.getElementById('set-antiafk-interval').value, 10);
  const secs = mins * 60;
  settings.antiAfkInterval = secs;
  api.saveSettings({ antiAfkInterval: secs });
  toast('Anti-AFK interval: ' + mins + ' min', 'ok');
}

// Silent trim; no toast/button spinner, just logs. Skips the tasklist
// round-trip entirely when nothing is running.
let _autoTrimTimer = null;
async function _autoTrimTick() {
  if (_mixRunning <= 0) return;
  let res;
  try { res = await api.trimRobloxMemory(); } catch { return; }
  if (res && res.ok && res.total > 0) logEntry('info', 'system', `Auto-trimmed ${res.trimmed}/${res.total} Roblox instance(s)`);
}
function startAutoTrimLoop() {
  stopAutoTrimLoop();
  const mins = settings.autoTrimIntervalMin || 5;
  _autoTrimTimer = setInterval(_autoTrimTick, mins * 60 * 1000);
}
function stopAutoTrimLoop() {
  if (_autoTrimTimer) { clearInterval(_autoTrimTimer); _autoTrimTimer = null; }
}
function toggleAutoRelaunch() {
  const el = document.getElementById('set-autorelaunch');
  const on = el.checked;
  settings.autoRelaunch = on;
  api.saveSettings({ autoRelaunch: on });
  toast(on ? 'Relaunch on disconnect on' : 'Relaunch on disconnect off', on ? 'ok' : 'err');
}
// Releasing/re-acquiring the mutex happens inside the helper and isn't
// instant, so re-read the real state afterwards instead of assuming the badge
// can be derived from the checkbox.
async function toggleMultiInstance() {
  const el = document.getElementById('set-multiinstance');
  const on = el.checked;
  el.disabled = true;
  settings.multiInstance = on;
  try {
    await api.saveSettings({ multiInstance: on });
    toast(on ? 'Multi-instance on' : 'Multi-instance off, Roblox will allow one client', on ? 'ok' : 'err');
  } finally {
    el.disabled = false;
  }
  await refreshMultiStatus();
}
function toggleProtectSignIn() {
  const on = document.getElementById('set-protectsignin').checked;
  settings.protectSignIn = on;
  api.saveSettings({ protectSignIn: on });
  toast(on ? 'Sign-in protection on' : 'Sign-in protection off', on ? 'ok' : 'err');
}
function toggleLowPriority() {
  const el = document.getElementById('set-lowpriority');
  const on = el.checked;
  settings.lowPriorityMultiInstance = on;
  api.saveSettings({ lowPriorityMultiInstance: on });
  toast(on ? 'Multi-instance priority lowering on' : 'Multi-instance priority lowering off', on ? 'ok' : 'err');
}
function toggleHideToTray() {
  const el = document.getElementById('set-hidetotray');
  const on = el.checked;
  settings.hideToTray = on;
  api.saveSettings({ hideToTray: on });
  // Persist first, then reconcile the icon: the backend reads the saved
  // setting on every close, and apply_setting only creates/removes the icon.
  api.setHideToTray(on);
  toast(on ? 'Closing will hide to the system tray' : 'Closing will quit MultiRoblox', on ? 'ok' : 'err');
}
function toggleAutoTrim() {
  const el = document.getElementById('set-autotrim');
  const on = el.checked;
  settings.autoTrim = on;
  api.saveSettings({ autoTrim: on });
  if (on) startAutoTrimLoop(); else stopAutoTrimLoop();
  toast(on ? 'Auto trim on' : 'Auto trim off', on ? 'ok' : 'err');
}
function autoTrimIntervalInput(v) {
  document.getElementById('set-autotrim-interval-val').textContent = v + ' min';
  updateSliderFill(document.getElementById('set-autotrim-interval'));
}
function autoTrimIntervalCommit() {
  const mins = parseInt(document.getElementById('set-autotrim-interval').value, 10);
  settings.autoTrimIntervalMin = mins;
  api.saveSettings({ autoTrimIntervalMin: mins });
  if (settings.autoTrim) startAutoTrimLoop();
  toast('Auto trim interval: ' + mins + ' min', 'ok');
}

// ── RAM limiter (Settings > Performance) ─────────────────────────────────
// 0 = off. The backend floors any real cap at 512 MB, so the slider's first
// step past Off is the smallest value that actually takes effect.
function ramLimitLabel(mb) {
  mb = parseInt(mb, 10) || 0;
  return mb > 0 ? formatMb(Math.max(512, mb)) : 'Off';
}
function ramLimitInput(v) {
  document.getElementById('set-ramlimit-val').textContent = ramLimitLabel(v);
  updateSliderFill(document.getElementById('set-ramlimit'));
}
async function ramLimitCommit() {
  let mb = parseInt(document.getElementById('set-ramlimit').value, 10) || 0;
  if (mb > 0 && mb < 512) mb = 512;
  // Keep the thumb on the value that was actually saved (256 rounds up).
  const ramSl = document.getElementById('set-ramlimit');
  if (parseInt(ramSl.value, 10) !== mb) { ramSl.value = mb; ramLimitInput(mb); }
  settings.ramLimitMb = mb;
  let saved = false;
  try { saved = await api.saveSettings({ ramLimitMb: mb }); } catch {}
  if (!saved) { toast('Could not save the RAM limit', 'err'); return; }
  logEntry('info', 'system', mb ? `RAM limit per instance set to ${mb} MB` : 'RAM limit per instance turned off', { limitMb: mb });
  toast(mb ? 'RAM limit: ' + formatMb(mb) + ' per instance' : 'RAM limit off', 'ok');
  pollMemoryUsage();
}

// ── Roblox install picker (Settings > Roblox) ─────────────────────────────
let _installs = [];
async function installsInit() {
  try { _installs = (await api.listInstalls()) || []; } catch { _installs = []; }
  const menu = document.getElementById('cdd-install-menu');
  if (!menu) return;
  const opt = (value, name, desc) =>
    `<div class="cdd-option" data-value="${esc(value)}" onclick="setRobloxInstall(${jsArg(value)})" role="button" tabindex="0">
      <div class="cdd-opt-left"><div class="cdd-opt-name">${esc(name)}</div><div class="cdd-opt-desc">${esc(desc)}</div></div>
      <span class="material-icons-round cdd-check">check</span>
    </div>`;
  const auto = _installs.find(i => i.autoPick) || _installs.find(i => i.active);
  menu.innerHTML = opt('', 'Auto', auto ? 'Currently ' + auto.name + (auto.latest ? ' (' + auto.latest + ')' : '') : 'Newest installed build')
    + _installs.map(i => opt(i.path, i.name,
        (i.kind === 'vanilla' ? 'Vanilla Roblox' : 'Bootstrapper')
        + (i.isDefault ? ' · default for Roblox links' : '')
        + (i.latest ? ' · ' + i.latest : '')
        + ' · ' + i.versions.length + ' build' + (i.versions.length === 1 ? '' : 's'))).join('');
  installUpdateUI(settings.robloxInstall || '');
  const desc = document.getElementById('install-desc');
  if (desc && !_installs.length) desc.textContent = 'No Roblox installation found. Install Roblox (or a bootstrapper) and reopen this tab.';
}
function installUpdateUI(path) {
  const found = _installs.find(i => i.path.toLowerCase() === String(path || '').toLowerCase());
  const label = document.getElementById('cdd-install-label');
  if (label) label.textContent = found ? found.name : 'Auto';
  document.querySelectorAll('#cdd-install-menu .cdd-option').forEach(o =>
    o.classList.toggle('selected', (o.dataset.value || '').toLowerCase() === (found ? found.path.toLowerCase() : '')));
}
async function setRobloxInstall(path) {
  closeAllCdd();
  settings.robloxInstall = path;
  let saved = false;
  try { saved = await api.saveSettings({ robloxInstall: path }); } catch {}
  if (!saved) { toast('Could not save the install choice', 'err'); return; }
  const found = _installs.find(i => i.path === path);
  installUpdateUI(path);
  logEntry('info', 'system', 'Roblox install set to ' + (found ? found.name + ' (' + found.path + ')' : 'Auto'));
  toast('Launches will use ' + (found ? found.name : 'the newest install'), 'ok');
  installsInit();
}

function toggleCdd(name) {
  const trigger = document.getElementById('cdd-' + name + '-trigger');
  const menu = document.getElementById('cdd-' + name + '-menu');
  const open = menu.classList.contains('open');
  closeAllCdd();
  if (open) return;
  // Open upward when the list would run past the bottom of the window.
  const box = trigger.getBoundingClientRect();
  const height = menu.offsetHeight + 16;
  menu.classList.toggle('up', box.bottom + height > window.innerHeight && box.top - height > 68);
  trigger.classList.add('open');
  menu.classList.add('open');
}
function closeAllCdd() {
  document.querySelectorAll('.cdd-trigger.open').forEach(t => t.classList.remove('open'));
  document.querySelectorAll('.cdd-menu.open').forEach(m => m.classList.remove('open'));
}
document.addEventListener('click', e => { if (!e.target.closest('.cdd')) closeAllCdd(); });

// Called after any navigation/tab switch so slider fills are always correct.
function refreshAllSliderFills() {
  document.querySelectorAll('.fps-slider').forEach(updateSliderFill);
}

// Segments aren't equal width, so the pill's left/width are computed in
// real pixels from the active button's own rect rather than assumed from
// index; this is also what lets one CSS rule work for both tab bars
// (Settings' 6 segments and Charts' 3) without hardcoding either count.
// Moves a selector pill to `to`. When it changes option it stretches across both
// options, then settles on the one it lands on.
// Same-option moves (window resizes) jump straight there.
const _reduceMotion = matchMedia('(prefers-reduced-motion: reduce)');
function movePill(pill, to, from, glide = true) {
  if (!from) {
    const cs = getComputedStyle(pill); // mid-move this is where it is right now
    from = { left: parseFloat(cs.left), top: parseFloat(cs.top), width: parseFloat(cs.width) };
  }
  const running = pill.getAnimations().filter(a => a.id === 'pill-move' && a.playState === 'running');
  const t = pill._to;
  if (running.length && t && Math.abs(t.left - to.left) < 1 && Math.abs(t.width - to.width) < 1 && (to.top == null || t.top == null || Math.abs(t.top - to.top) < 1)) return; // already heading there
  running.forEach(a => a.cancel());
  pill._to = to;
  pill.style.left = to.left + 'px';
  pill.style.width = to.width + 'px';
  if (to.top != null) pill.style.top = to.top + 'px';
  if (to.height != null) pill.style.height = to.height + 'px';
  const still = Math.abs(from.left - to.left) < 1 && Math.abs(from.width - to.width) < 1;
  if (!glide || still || _reduceMotion.matches || !to.width || ![from.left, from.width].every(Number.isFinite)) return;
  // The pill stretches out toward the new option, then its trailing edge catches up.
  const spanL = Math.min(from.left, to.left);
  const spanW = Math.max(from.left + from.width, to.left + to.width) - spanL;
  const frames = [
    { left: from.left + 'px', width: from.width + 'px' },
    { offset: 0.4, left: spanL + 'px', width: spanW + 'px' },
    { left: to.left + 'px', width: to.width + 'px' },
  ];
  if (to.top != null && Number.isFinite(from.top) && Math.abs(from.top - to.top) >= 1) {
    [from.top, (from.top + to.top) / 2, to.top].forEach((t, i) => { frames[i].top = t + 'px'; });
  }
  pill.animate(frames, { id: 'pill-move', duration: 360, easing: 'cubic-bezier(.32,.72,.2,1)' });
}

// ── Tab motion (after RoALT, MIT; see THIRD_PARTY_NOTICES.md) ──
// A press on a tab-style selector leaves ink where it landed. The look is in styles.css;
// this only places the ink.
const INK_HOSTS = '.nav-item, .vt, .tab-btn, .gv-seg button';
// Where the pointer is inside `el`, in the element's own pixels (a pressed option is scaled).
function localPoint(el, e) {
  const r = el.getBoundingClientRect();
  const s = el.offsetWidth ? r.width / el.offsetWidth : 1;
  return { x: (e.clientX - r.left) / s, y: (e.clientY - r.top) / s };
}
function inkAt(el, e) {
  const p = localPoint(el, e);
  let box = el.querySelector(':scope > .lq-ink');
  if (!box) {
    if (getComputedStyle(el).position === 'static') el.style.position = 'relative';
    box = document.createElement('span');
    box.className = 'lq-ink';
    box.setAttribute('aria-hidden', 'true');
    el.append(box);
  }
  const drop = document.createElement('span');
  drop.className = 'lq-ripple';
  drop.style.left = p.x + 'px';
  drop.style.top = p.y + 'px';
  box.append(drop);
  drop.addEventListener('animationend', () => drop.remove(), { once: true });
  setTimeout(() => drop.remove(), 900); // if nothing animates, nothing would end it
}
document.addEventListener('pointerdown', e => {
  return; // press ripple retired; the pill stretch is the tab motion now
  if (e.button !== 0 || !(e.target instanceof Element) || _reduceMotion.matches) return;
  const hit = e.target.closest(INK_HOSTS);
  if (hit && !hit.matches(':disabled, [aria-disabled=true]')) inkAt(hit, e);
}, true);

// White pill that slides between a selector's options. The game page rebuilds its
// selectors on every change, so the last spot is remembered to slide from it.
const _segLast = {};
function syncSeg(seg, key = seg?.id) {
  const active = seg?.querySelector(':scope > button.active');
  if (!active || !active.offsetWidth) return;
  let pill = seg.querySelector(':scope > .seg-pill');
  const isNew = !pill;
  if (isNew) {
    pill = document.createElement('span');
    pill.className = 'seg-pill';
    seg.prepend(pill);
  }
  const to = { left: active.offsetLeft, top: active.offsetTop, width: active.offsetWidth, height: active.offsetHeight };
  if (isNew) movePill(pill, to, _segLast[key] || to);
  else movePill(pill, to, null, pill._on !== active);
  pill._on = active;
  _segLast[key] = to;
}
function syncStaticSegs() {
  ['log-filter', 'edit-tabs', 'login-tabs', 'acct-view'].forEach(id => syncSeg(document.getElementById(id)));
}
document.fonts?.ready.then(syncStaticSegs);
window.addEventListener('resize', syncStaticSegs);

function positionTabSlider(barEl) {
  if (!barEl) return;
  let slider = barEl.querySelector('.tab-slider');
  const isNew = !slider;
  if (isNew) {
    slider = document.createElement('div');
    slider.className = 'tab-slider';
    barEl.insertBefore(slider, barEl.firstChild);
  }
  const active = barEl.querySelector('.tab-btn.active');
  if (!active) { slider.style.opacity = '0'; return; }
  // offsetLeft/offsetWidth, not getBoundingClientRect: they're relative to
  // the padding edge of the nearest positioned ancestor (here, barEl itself,
  // since both are its direct children); exactly what CSS `left` on an
  // absolutely-positioned sibling measures from. getBoundingClientRect
  // instead gives the border-box's outer edge, so subtracting two of those
  // was off by the bar's own border-width in every direction: a small,
  // constant, one-sided-looking gap.
  movePill(slider, { left: active.offsetLeft, width: active.offsetWidth }, null, !isNew && slider._on !== active);
  slider._on = active;
  slider.style.opacity = '1';
}
// Same technique as positionTabSlider, vertical instead of horizontal; nav
// items are flex-stacked with gaps rather than a fixed row height, so top/
// height come from the active item's measured rect, not an assumed index.
window.addEventListener('resize', () => {
  document.querySelectorAll('.tab-bar').forEach(bar => {
    if (bar.querySelector('.tab-btn.active')) positionTabSlider(bar);
  });
});

function settingsTab(tab) {
  ['general','performance','roblox','privacy','themes'].forEach(t => {
    const panel = document.getElementById('stab-panel-' + t);
    const btn = document.getElementById('stab-' + t);
    if (panel) panel.style.display = t === tab ? '' : 'none';
    if (btn) btn.classList.toggle('active', t === tab);
  });
  if (tab === 'performance') renderEngineInit();
  if (tab === 'roblox') installsInit();
  refreshAllSliderFills();
  positionTabSlider(document.getElementById('stab-general')?.closest('.tab-bar'));
}

function goTo(p) {
  if (p === 'themes') { goTo('settings'); settingsTab(p); return; }
  closeGameView();
  document.querySelectorAll('.page').forEach(x => x.classList.remove('active'));
  document.querySelectorAll('.nav-item').forEach(x => x.classList.remove('active'));
  document.getElementById('page-' + p).classList.add('active');
  const navItem = document.getElementById('nav-' + p);
  navItem.classList.add('active');
  placeNavPill(navItem);
  if (p === 'settings') {
    document.getElementById('stat-count').textContent = accounts.length;
    refreshMultiStatus();
    // Covers landing here without ever clicking a tab; the default-active
    // tab still needs the pill positioned under it at least once.
    positionTabSlider(document.getElementById('stab-general')?.closest('.tab-bar'));
  }
  if (p === 'logs') { renderLogs(); syncStaticSegs(); }
  if (p === 'charts') {
    chartsOnEnter();
    if (!chartsLoaded) loadCharts();
    positionTabSlider(document.getElementById('ctab-popular')?.closest('.tab-bar'));
  } else {
    // Start the countdown to dropping the charts payload. Bouncing to another
    // tab and straight back inside the window costs nothing.
    chartsOnLeave();
  }
  if (p === 'packages') renderPackages();
  if (p === 'accounts') pollSystemStats();
  if (p === 'mixer') mixInit();
  if (p === 'tracking') renderTrackingPage();
  // generator page
  refreshAllSliderFills();
}

// One tooltip shared by the whole nav bar. It fades in under the first icon you
// reach and then glides between icons while the pointer stays on the bar.
(function initNavTip() {
  const nav = document.getElementById('topnav'), tip = document.getElementById('nav-tip');
  let showTimer = null;
  const hide = () => { clearTimeout(showTimer); tip.classList.remove('show'); };
  const placeOver = item => {
    tip.textContent = item.querySelector('.nav-label').textContent;
    tip.style.setProperty('--x', Math.round(item.offsetLeft + (item.offsetWidth - tip.offsetWidth) / 2) + 'px');
  };
  function showFor(item) {
    clearTimeout(showTimer);
    if (tip.classList.contains('show')) { placeOver(item); return; }
    showTimer = setTimeout(() => {
      tip.style.transition = 'none'; // start under this icon, not sliding in from the last one
      placeOver(item);
      void tip.offsetWidth;
      tip.style.transition = '';
      tip.classList.add('show');
    }, 120);
  }
  nav.querySelectorAll('.nav-item').forEach(item => {
    item.addEventListener('mouseenter', () => showFor(item));
    item.addEventListener('click', hide);
    item.addEventListener('focus', () => { if (item.matches(':focus-visible')) showFor(item); });
    item.addEventListener('blur', hide);
  });
  nav.addEventListener('mouseleave', hide);
})();

// Wide windows show every page's name, so the pill takes the active item's width too.
function placeNavPill(item = document.querySelector('#topnav .nav-item.active')) {
  if (!item) return;
  const pill = document.getElementById('nav-pill');
  movePill(pill, { left: item.offsetLeft, width: item.offsetWidth }, null, !!pill._on && pill._on !== item);
  pill._on = item;
}
placeNavPill();
window.addEventListener('resize', () => placeNavPill());
document.fonts?.ready.then(() => placeNavPill());

// When each running account started, kept across app restarts so the timer on a
// client that is still open doesn't reset to zero.
let _liveSince = {};
try { _liveSince = JSON.parse(localStorage.getItem('live-since')) || {}; } catch {}

function syncRunTimes() {
  let changed = false;
  _launchedIds.forEach(id => { if (!_liveSince[id]) { _liveSince[id] = Date.now(); changed = true; } });
  Object.keys(_liveSince).forEach(id => { if (!_launchedIds.has(id)) { delete _liveSince[id]; changed = true; } });
  if (changed) { try { localStorage.setItem('live-since', JSON.stringify(_liveSince)); } catch {} }
  paintRunTimes();
}

function paintRunTimes() {
  document.querySelectorAll('.card.is-live .card-av').forEach(av => {
    const since = _liveSince[av.closest('.card').dataset.id];
    const mins = since ? Math.floor((Date.now() - since) / 60000) : 0;
    av.dataset.run = mins < 1 ? 'Running' : mins < 60 ? `Running ${mins}m` : `Running ${Math.floor(mins / 60)}h ${mins % 60}m`;
  });
}

function markLaunched(id) {
  if (!_launchedIds.has(id)) relayoutSoon(id);
  _launchedIds.add(id);
  syncRunTimes();
  const card = document.querySelector(`.card[data-id="${id}"]`);
  if (card) {
    card.classList.add('is-live');
    const dot = card.querySelector('.card-dot');
    if (dot) { dot.classList.add('launched'); dot.title = 'Launched'; }

  }
  refreshPkgAvatarStatus();
}

async function killOne(id) {
  const a = accounts.find(x => x.id === id);
  logEntry('warn', 'kill', `Killing Roblox instance for ${a ? a.username : id}...`, { accountId: id, username: a?.username, userId: a?.userId });
  let res;
  try { res = await api.killOneRoblox(id); } catch (e) { res = { ok: false, error: e?.message || String(e) }; }
  if (!res || !res.ok) toast(res?.error || 'Could not kill that instance', 'err');
  else logEntry('ok', 'kill', `Killed instance for ${a ? a.username : id}`, { accountId: id });
}

// ── Card context menu ─────────────────────────────────────────────────────
let _ctxMenuId = null;
function showCardMenu(id, x, y) {
  closeCardMenu();
  _ctxMenuId = id;
  const a = accounts.find(x => x.id === id);
  const isLive = _launchedIds.has(id);
  const menu = document.createElement('div');
  menu.id = 'card-ctx-menu';
  menu.className = 'ctx-menu';
  menu.innerHTML = `
    <div class="ctx-header">${esc(a ? (a.nickname || a.username || 'Unknown') : id)}</div>
    ${isLive ? `<button class="ctx-item ctx-danger" onclick="ctxKill(${jsArg(id)})"><span class="material-icons-round">power_settings_new</span>Kill instance</button>` : ''}
    ${isLive ? `<button class="ctx-item" onclick="ctxTrim(${jsArg(id)})"><span class="material-icons-round">memory</span>Trim memory</button>` : ''}
    ${isLive ? `
    <div class="ctx-item ctx-has-sub">
      <span class="material-icons-round">speed</span>Set priority
      <span class="material-icons-round ctx-sub-arrow">chevron_right</span>
      <div class="ctx-submenu">
        ${['realtime', 'high', 'abovenormal', 'normal', 'belownormal', 'low'].map(p =>
          `<button class="ctx-item${_accountPriority[id] === p ? ' ctx-active' : ''}" onclick="ctxSetPriority(${jsArg(id)},'${p}')">${PRIORITY_LABELS[p]}</button>`
        ).join('')}
      </div>
    </div>` : ''}
    ${isLive ? '' : `<button class="ctx-item" onclick="ctxLaunch(${jsArg(id)})"><span class="material-icons-round">rocket_launch</span>Launch</button>`}
    <button class="ctx-item" onclick="ctxEdit(${jsArg(id)})"><span class="material-icons-round">edit</span>Edit account</button>
    <button class="ctx-item" onclick="ctxToggleMain(${jsArg(id)})"><span class="material-icons-round">star</span>${a && a.tag === 'MAIN' ? 'Remove MAIN tag' : 'Mark as MAIN'}</button>
    <div class="ctx-sep"></div>
    <button class="ctx-item" onclick="ctxCopyId(${jsArg(id)})"><span class="material-icons-round">tag</span>Copy user ID</button>
    <button class="ctx-item" onclick="ctxCopyUser(${jsArg(id)})"><span class="material-icons-round">person</span>Copy username</button>
    <button class="ctx-item" onclick="ctxCopyCookie(${jsArg(id)})"><span class="material-icons-round">cookie</span>Copy cookie</button>
    <button class="ctx-item" onclick="ctxOpenBrowser(${jsArg(id)})"><span class="material-icons-round">open_in_browser</span>Open in browser</button>
  `;
  document.body.appendChild(menu);
  positionCardMenu(menu, x, y);
  setTimeout(() => document.addEventListener('click', closeCardMenu, { once: true }), 0);
}

const CTX_EDGE_GAP = 8;

// Keeps the menu (and its submenu) fully on screen. Previously this assumed a
// 200px-wide menu and only clamped the far edge, so a menu wider than that
// (the width grows with the account name and the "Set priority" row) still
// ran off the right, and a clamp that went negative pushed the top of the menu
// above the viewport where it couldn't be reached.
function positionCardMenu(menu, x, y) {
  const vw = window.innerWidth, vh = window.innerHeight;
  const rect = menu.getBoundingClientRect();
  let h = rect.height;

  // Defensive: a menu taller than the window would otherwise have to be
  // positioned partly off-screen. Scroll it instead of hiding items.
  if (h > vh - CTX_EDGE_GAP * 2) {
    h = vh - CTX_EDGE_GAP * 2;
    menu.style.maxHeight = h + 'px';
    menu.style.overflowY = 'auto';
  }

  // Clamp both ends: Math.min alone keeps it off the right/bottom edge but
  // happily produces a negative left/top on a small window.
  menu.style.left = Math.max(CTX_EDGE_GAP, Math.min(x, vw - rect.width - CTX_EDGE_GAP)) + 'px';
  menu.style.top = Math.max(CTX_EDGE_GAP, Math.min(y, vh - h - CTX_EDGE_GAP)) + 'px';

  positionCardSubmenu(menu, vw, vh);
}

// The priority submenu opens to the right at left:100%. Near the right edge
// that put it entirely outside the window; the row highlighted on hover but
// nothing appeared, which is the "menu goes invisible" case. It's laid out
// (opacity:0, not display:none), so it can be measured before it's shown.
function positionCardSubmenu(menu, vw, vh) {
  const parent = menu.querySelector('.ctx-has-sub');
  const sub = parent && parent.querySelector('.ctx-submenu');
  if (!sub) return;

  const menuRect = menu.getBoundingClientRect();
  const subRect = sub.getBoundingClientRect();

  // Flip to the left of the menu when there isn't room on the right, unless
  // there's even less room on that side.
  const roomRight = vw - menuRect.right;
  const roomLeft = menuRect.left;
  if (roomRight < subRect.width + CTX_EDGE_GAP && roomLeft > roomRight) {
    parent.classList.add('ctx-flip-x');
  }

  // And lift it when it would run past the bottom of the window.
  const overflowBelow = parent.getBoundingClientRect().top - 4 + subRect.height + CTX_EDGE_GAP - vh;
  if (overflowBelow > 0) sub.style.top = (-4 - overflowBelow) + 'px';
}
function closeCardMenu() { const m = document.getElementById('card-ctx-menu'); if (m) m.remove(); _ctxMenuId = null; }
async function ctxKill(id) { closeCardMenu(); await killOne(id); }
async function ctxTrim(id) {
  closeCardMenu();
  const a = accounts.find(x => x.id === id);
  let res;
  try { res = await api.trimAccountMemory(id); } catch { res = null; }
  if (res?.ok) {
    logEntry('info', 'system', `Trimmed memory for ${a?.username || id}`, { accountId: id, username: a?.username, userId: a?.userId });
    toast(`Trimmed memory for ${a?.nickname || a?.username || 'instance'}`, 'ok');
  } else {
    toast(res?.error || 'Could not trim this instance', 'err');
  }
}
const PRIORITY_LABELS = { realtime: 'Realtime', high: 'High', abovenormal: 'Above normal', normal: 'Normal', belownormal: 'Below normal', low: 'Low' };
const _accountPriority = {}; // id -> last priority manually set this session, drives the ctx-active highlight
async function ctxSetPriority(id, priority) {
  closeCardMenu();
  const a = accounts.find(x => x.id === id);
  let res;
  try { res = await api.setAccountPriority(id, priority); } catch { res = null; }
  if (res?.ok) {
    _accountPriority[id] = priority;
    toast(`Priority set to ${PRIORITY_LABELS[priority]} for ${a?.nickname || a?.username || 'instance'}`, 'ok');
  } else {
    toast(res?.error || 'Could not set priority', 'err');
  }
}

function ctxLaunch(id) { closeCardMenu(); startAccount(id); }
async function ctxToggleMain(id) {
  closeCardMenu();
  const a = accounts.find(x => x.id === id);
  if (!a) return;
  let updated;
  try { updated = await api.updateAccount(id, { tag: a.tag === 'MAIN' ? '' : 'MAIN' }); }
  catch (e) { toast('Could not save the tag: ' + (e?.message || e), 'err'); return; }
  const idx = accounts.findIndex(x => x.id === id);
  if (updated && idx !== -1) { accounts[idx] = updated; render(); }
}
function ctxEdit(id) { closeCardMenu(); openEdit(id); }
function ctxCopyId(id) { closeCardMenu(); const a = accounts.find(x => x.id === id); if (a?.userId) navigator.clipboard.writeText(a.userId).then(() => toast('User ID copied', 'ok')); else toast('No user ID', 'err'); }
function ctxCopyUser(id) { closeCardMenu(); const a = accounts.find(x => x.id === id); if (a?.username) navigator.clipboard.writeText(a.username).then(() => toast('Username copied', 'ok')); else toast('No username', 'err'); }
function ctxCopyCookie(id) { closeCardMenu(); const a = accounts.find(x => x.id === id); if (a?.cookie) navigator.clipboard.writeText(a.cookie).then(() => toast('Cookie copied', 'ok')); else toast('No cookie saved for this account', 'err'); }
async function ctxOpenBrowser(id) {
  closeCardMenu();
  const a = accounts.find(x => x.id === id);
  if (!a?.cookie) { toast('No cookie saved for this account', 'err'); return; }
  toast('Opening browser…', 'ok');
  let res;
  try { res = await api.openAccountInBrowser(a.cookie); } catch { res = null; }
  if (!res || !res.ok) toast((res && res.error) || 'Could not open browser', 'err');
}

function refreshPkgAvatarStatus() {
  document.querySelectorAll('.pkg-avatar[data-acc-id]').forEach(av => {
    av.classList.toggle('online', _launchedIds.has(av.dataset.accId));
  });
  // Keeps each group's Kill button in sync even when instances start/stop
  // outside launchPackage/killPackage.
  packages.forEach(p => {
    const btn = document.querySelector('.pkg-card[data-id="' + p.id + '"] .pkg-kill-btn');
    if (!btn || btn.dataset.busy === '1') return;
    const anyRunning = (p.accountIds || []).some(id => _launchedIds.has(id));
    btn.disabled = !anyRunning;
  });
  packages.forEach(p => {
    const card = document.querySelector('.pkg-card[data-id="' + p.id + '"]');
    if (!card) return;
    const ids = (p.accountIds || []).filter(id => accounts.some(a => a.id === id));
    const running = ids.filter(id => _launchedIds.has(id)).length;
    const meta = card.querySelector('.pkg-meta');
    meta.textContent = ids.length ? `${running}/${ids.length} running` : 'No accounts';
    meta.classList.toggle('live', running > 0);
    card.querySelector('.pkg-run-bar span').style.width = ids.length ? (running / ids.length * 100) + '%' : '0';
  });
}

let _gridAnimate = true; // first paint, and when the view or filter changes
function render() {
  const grid = document.getElementById('grid'), empty = document.getElementById('empty');
  paintAcctSub();
  if (!accounts.length) { grid.innerHTML = ''; empty.style.display = 'flex'; paintWelcome(); return; }
  empty.style.display = 'none';
  const list = visibleAccounts();
  grid.classList.toggle('list-view', _acctView === 'list');
  const animate = _gridAnimate;
  grid.classList.toggle('animate', animate);
  _gridAnimate = false;
  if (!list.length) {
    grid.classList.remove('list-view');
    grid.innerHTML = '<div style="grid-column:1/-1;text-align:center;color:var(--t3);font-size:var(--fs-sm);padding:40px 0">No accounts match your search or filter.</div>';
    return;
  }
  // Each card's markup doubles as its fingerprint. A card whose fingerprint hasn't
  // changed stays on screen as it is (avatar, readouts, a half-typed username), so
  // a change to one account redraws one card instead of all of them. The avatar's
  // loading state and the typed username are left out of the fingerprint: both
  // change on their own without the card needing a redraw.
  const AV_MARK = '', DRAFT_MARK = '';
  const cards = list.map(a => {
    const key = cardMarkup(a, AV_MARK, DRAFT_MARK);
    return { a, key, html: key.replace(AV_MARK, a.userId && !_avatarCache[a.userId] ? ' av-loading' : '').replace(DRAFT_MARK, esc(_followDraft[a.id] || '')) };
  });
  const addHtml = `<div class="card-add" onclick="openLogin()"><span class="material-icons-round card-add-icon">add</span><span class="card-add-label">Add account</span></div>`;
  const onScreen = new Map([...grid.querySelectorAll(':scope > .card[data-id]')].map(c => [c.dataset.id, c]));
  let fresh;
  if (animate || !onScreen.size) {
    grid.innerHTML = cards.map((c, i) => c.html.replace('<div class="card', `<div style="animation-delay:${Math.min(i, 14) * 24}ms" class="card`)).join('') + addHtml;
    fresh = [...grid.querySelectorAll(':scope > .card[data-id]')];
    fresh.forEach((el, i) => { el._key = cards[i].key; });
  } else {
    fresh = [];
    const nodes = cards.map(c => {
      const kept = onScreen.get(c.a.id);
      if (kept && kept._key === c.key) { onScreen.delete(c.a.id); return kept; }
      const tpl = document.createElement('template');
      tpl.innerHTML = c.html.trim();
      const el = tpl.content.firstElementChild;
      el._key = c.key;
      fresh.push(el);
      return el;
    });
    onScreen.forEach(el => el.remove()); // gone, or about to be replaced
    let addEl = grid.querySelector(':scope > .card-add');
    if (!addEl) { const tpl = document.createElement('template'); tpl.innerHTML = addHtml; addEl = tpl.content.firstElementChild; }
    // Only move what is out of place: moving a card would drop focus from its box.
    let cursor = grid.firstElementChild;
    for (const el of [...nodes, addEl]) {
      if (el === cursor) { cursor = cursor.nextElementSibling; continue; }
      grid.insertBefore(el, cursor);
    }
    while (cursor) { const next = cursor.nextElementSibling; cursor.remove(); cursor = next; }
  }
  loadAvatarsBatch(list.filter(a => !document.querySelector(`#av-${CSS.escape(a.id)} img`)));
  list.forEach(a => { if (a.gameTarget && !_gameNameCache[a.id]) fetchGameName(a.id, a.gameTarget); });
  checkCookieHealth(list);
  fresh.forEach(card => {
    card.addEventListener('contextmenu', e => { e.preventDefault(); showCardMenu(card.dataset.id, e.clientX, e.clientY); });
  });
  initDrag(fresh);
  paintMemUsage(); // new cards start with empty readouts
  paintPreviews();
  // A picture that never arrives falls back to the account's letter.
  setTimeout(() => document.querySelectorAll('.card-av.av-loading').forEach(av => av.classList.remove('av-loading')), 8000);
}

function cardMarkup(a, avMark, draftMark) {
  return `
    <div class="card${_launchedIds.has(a.id) ? ' is-live' : ''}${_launchingIds.has(a.id) ? ' is-launching' : ''}${_cookieStatus[a.id] === 'dead' ? ' cookie-dead' : ''}" data-id="${a.id}">
      <div class="card-dot${_launchedIds.has(a.id) ? ' launched' : ''}" title="${_launchedIds.has(a.id) ? 'Launched' : 'Not launched'}"></div>
      <span class="material-icons-round drag-handle">drag_indicator</span>
      <div class="card-av${avMark}" id="av-${a.id}" data-run="Running">${esc((a.username || '?')[0].toUpperCase())}</div>
      <div class="card-id">
        <div class="card-name-row">
          <div class="card-name">${esc(a.nickname || a.username || 'Unknown')}</div>
          ${a.tag ? `<span class="card-tag${a.tag === 'MAIN' ? ' main' : ''}">${esc(a.tag)}</span>` : ''}
          <span class="card-expired" title="This account's cookie is no longer valid. Re-add the account to refresh it."><span class="material-icons-round">error_outline</span>Expired</span>
        </div>
        <div class="card-uid">${a.userId ? 'ID ' + esc(a.userId) : 'No ID'}</div>
      </div>
      <div class="card-game visible${a.gameTarget ? '' : ' none'}" id="gt-${a.id}" title="${esc(a.gameTarget || 'No game set. Edit the account to pick one')}">${a.gameTarget ? esc(truncate(_gameNameCache[a.id] || extractTargetLabel(a.gameTarget), 22)) : 'No game set'}</div>
      <div class="card-mem" id="mem-${esc(a.id)}"></div>
      <div class="card-idle-note">Not running</div>
      <div class="card-follow">
        <label class="card-follow-label" for="follow-${esc(a.id)}">Username</label>
        <input class="card-follow-input" id="follow-${esc(a.id)}" type="text" spellcheck="false" autocomplete="off" aria-label="Username to follow"
          value="${draftMark}" oninput="_followDraft[${jsArg(a.id)}]=this.value"
          onkeydown="if(event.key==='Enter'){event.preventDefault();followFromRow(${jsArg(a.id)});}"/>
        <button class="btn btn-ghost card-follow-btn" onclick="followFromRow(${jsArg(a.id)})" title="Join the server this user is in">Follow</button>
        <span class="card-follow-sep"></span>
      </div>
      <div class="card-row">
        <button class="btn btn-launch" onclick="cardPrimaryAction(${jsArg(a.id)})">
          <span class="lbl-start">Start</span><span class="lbl-stop">Stop</span><span class="lbl-wait"><span class="spin"></span>Starting</span>
        </button>
        <button class="btn btn-edit" onclick="openEdit(${jsArg(a.id)})" aria-label="Edit account">
          <span class="material-icons-round">edit</span>
        </button>
        <button class="btn btn-del" onclick="removeAcc(${jsArg(a.id)})" aria-label="Remove account">
          <span class="material-icons-round">delete_outline</span>
        </button>
      </div>
    </div>`;
}

const _cookieStatus = {}; // id -> 'checking' | 'ok' | 'dead' | 'unknown'
function applyCookieStatus(id) {
  const card = document.querySelector(`.card[data-id="${id}"]`);
  if (card) card.classList.toggle('cookie-dead', _cookieStatus[id] === 'dead');
}
// Surfaces a likely-expired cookie right after a launch auth failure, without
// waiting for the periodic health check. The error text alone only says it's
// worth checking: a launch can fail on a network blip or a rate limit with a
// message that mentions the cookie, and that used to mark healthy accounts
// EXPIRED. Roblox's own answer decides the badge.
function _flagCookieMaybeDead(id, error) {
  if (!id || !error || !/cookie|expired|\b403\b/i.test(error)) return;
  const acc = accounts.find(a => a.id === id);
  if (!acc || !acc.cookie) return;
  api.validateCookie(acc.cookie).then(res => {
    if (res && !res.ok && res.reachable !== false) {
      _cookieStatus[id] = 'dead';
      applyCookieStatus(id);
    }
  }).catch(() => {});
}

// Strips what commonly travels with a cookie value: surrounding whitespace and
// quotes, the name= prefix, and trailing attributes. Sent as it was, any of
// these makes Roblox reject a perfectly good cookie as if it had expired.
function normalizeCookie(raw) {
  const unquote = s => s.trim().replace(/^["']|["']$/g, '').trim();
  return unquote(unquote(String(raw || '')).replace(/^\.?ROBLOSECURITY=/, '').split(';')[0]);
}

// Runs `worker` over `items` with at most `limit` in flight at once.
async function _runWithConcurrency(items, limit, worker) {
  let next = 0;
  async function lane() {
    while (next < items.length) {
      const item = items[next++];
      await worker(item);
    }
  }
  await Promise.all(Array.from({ length: Math.min(limit, items.length) }, lane));
}

let _cookieCheckRunning = false;
async function checkCookieHealth(list) {
  if (_cookieCheckRunning) return;
  // Never checked, or given a new cookie since the last check (re-added after expiring).
  const due = a => a.cookie && (_cookieStatus[a.id] === undefined || (_cookieCheckedWith[a.id] !== undefined && _cookieCheckedWith[a.id] !== a.cookie));
  const todo = list.filter(due);
  if (!todo.length) return;
  _cookieCheckRunning = true;
  try {
    await _runWithConcurrency(todo, 4, async (a) => {
      if (!due(a)) return;
      _cookieStatus[a.id] = 'checking';
      try {
        const res = await api.validateCookie(a.cookie);
        _cookieCheckedAt[a.id] = Date.now();
        _cookieCheckedWith[a.id] = a.cookie;
        // reachable:false means we never got an answer out of Roblox (DNS,
        // timeout, no connection), which says nothing about the cookie.
        // Marking those 'dead' turned any brief network drop into every
        // account showing EXPIRED. Leave them unknown so the next pass
        // re-checks instead of accusing a good cookie.
        const st = (res && res.ok) ? 'ok' : (res && res.reachable === false ? 'unknown' : 'dead');
        _cookieStatus[a.id] = st;
        if (st === 'dead') logEntry('warn', 'cookie', `Cookie invalid for ${a.username || a.id}${res && res.reason ? ': ' + res.reason : ''}`, { accountId: a.id, username: a.username || null, userId: a.userId || null });
        else if (st === 'unknown') logEntry('warn', 'cookie', `Could not reach Roblox to check ${a.username || a.id}`, { accountId: a.id, username: a.username || null, userId: a.userId || null });
        else logEntry('info', 'cookie', `Cookie valid for ${a.username || a.id}`, { accountId: a.id, username: a.username || null, userId: a.userId || null });
      } catch { _cookieStatus[a.id] = 'unknown'; logEntry('warn', 'cookie', `Cookie check failed for ${a.username || a.id}`, { accountId: a.id }); }
      applyCookieStatus(a.id);
    });
  } finally { _cookieCheckRunning = false; }
}

// Recheck ALL cookies every 60s so status stays live
let _recheckRunning = false;
const _cookieCheckedAt = {};            // id -> last validation epoch ms
const _cookieCheckedWith = {};          // id -> the cookie that check was for
const OK_RECHECK_MS = 5 * 60 * 1000;    // re-check known-good cookies at most every 5 min
// An expired cookie doesn't come back by itself, and asking Roblox about it every
// minute only burns the rate limit the other checks need. A new cookie for the
// account is checked straight away regardless.
const DEAD_RECHECK_MS = 10 * 60 * 1000;
async function recheckAllCookies(force) {
  if (_recheckRunning) return; // bail if a previous pass is still going
  _recheckRunning = true;
  // Flag before the first await so checkCookieHealth's render() pass doesn't race us.
  for (const a of accounts) if (a.cookie && _cookieStatus[a.id] === undefined) _cookieStatus[a.id] = 'checking';
  try {
  let changed = false;
  const now = Date.now();
  const todo = accounts.filter(a => {
    if (!a.cookie) return false;
    // Known-good cookies recheck every 5 minutes, expired ones every 10, and ones
    // Roblox couldn't be reached for on every tick.
    if (force || !_cookieCheckedAt[a.id] || _cookieCheckedWith[a.id] !== a.cookie) return true;
    const wait = { ok: OK_RECHECK_MS, dead: DEAD_RECHECK_MS }[_cookieStatus[a.id]] || 0;
    return now - _cookieCheckedAt[a.id] >= wait;
  });
  await _runWithConcurrency(todo, 4, async (a) => {
    const prev = _cookieStatus[a.id];
    _cookieStatus[a.id] = 'checking';
    try {
      const res = await api.validateCookie(a.cookie);
      _cookieCheckedAt[a.id] = Date.now();
      _cookieCheckedWith[a.id] = a.cookie;
      // Roblox unreachable tells us nothing about the cookie. This pass runs
      // every 60s, so treating a transport failure as 'dead' meant one blip
      // flipped every account to EXPIRED. Hold the previous verdict instead.
      if (res && !res.ok && res.reachable === false) {
        _cookieStatus[a.id] = prev || 'unknown';
        return;
      }
      const next = (res && res.ok) ? 'ok' : 'dead';
      if (next !== prev) {
        _cookieStatus[a.id] = next;
        applyCookieStatus(a.id); // toggles .cookie-dead on the card (badge + ring)
        changed = true;
        if (next === 'dead') logEntry('warn', 'cookie', `Cookie expired for ${a.username || a.id}${res && res.reason ? ': ' + res.reason : ''}`, { accountId: a.id, username: a.username, userId: a.userId });
        else if (prev === 'dead' && next === 'ok') logEntry('ok', 'cookie', `Cookie re-validated for ${a.username || a.id}`, { accountId: a.id, username: a.username, userId: a.userId });
      } else {
        _cookieStatus[a.id] = next;
      }
    } catch { _cookieStatus[a.id] = prev || 'unknown'; }
  });
  if (changed) render(); // rebuild once at the end so the cards match
  } finally { _recheckRunning = false; }
}
setInterval(() => { if (accounts.length) recheckAllCookies(false); }, 60000);


const _gameNameCache = {}; // accountId -> resolved game name
const _gameNameInflight = new Set(); // accountId + target lookups still pending
let _gameNamePersist = {}; // target -> resolved name, persisted to localStorage
try { _gameNamePersist = JSON.parse(localStorage.getItem('mr-gamenames') || '{}'); } catch { _gameNamePersist = {}; }
function _saveGameNames() { try { localStorage.setItem('mr-gamenames', JSON.stringify(_gameNamePersist)); } catch {} }

function extractTargetLabel(target) {
  if (!target) return '';
  const t = target.trim();
  if (/^\d+$/.test(t)) return t;
  try {
    const u = new URL(t.startsWith('http') ? t : 'https://' + t);
    const parts = u.pathname.split('/').filter(Boolean);
    // extract linkCode or share code for private servers
    const name = (parts[2] || parts[1] || '').replace(/-/g, ' ').trim();
    return name || 'Server link';
  } catch { return truncate(target, 22); }
}

async function fetchGameName(accountId, target) {
  if (!target) return;
  const t = target.trim();
  // Persistent cache hit: skip the network entirely.
  if (_gameNamePersist[t]) {
    _gameNameCache[accountId] = _gameNamePersist[t];
    updateGameLabel(accountId);
    return;
  }
  // Find the account to get its cookie for authenticated requests
  const acct = accounts.find(a => a.id === accountId);
  const cookie = acct ? acct.cookie : null;
  let placeId = null;
  if (/^\d+$/.test(t)) {
    placeId = t;
  } else {
    try {
      const u = new URL(t.startsWith('http') ? t : 'https://' + t);
      const parts = u.pathname.split('/').filter(Boolean);
      // /games/<placeId>/... or /games/<placeId>
      if (parts[0] === 'games' && parts[1] && /^\d+$/.test(parts[1])) placeId = parts[1];
      if (!placeId) placeId = u.searchParams.get('placeId');
      // PlaceLauncher URLs: ?placeId=...
      if (!placeId) { const m = t.match(/[?&]placeId=(\d+)/); if (m) placeId = m[1]; }
    } catch {}
  }
  if (!cookie) {
    _gameNameCache[accountId] = extractTargetLabel(target);
    updateGameLabel(accountId);
    return;
  }
  // render() calls this for every card still missing a name, and search
  // re-renders on each keystroke; without this every render fired another
  // lookup for the same account while the first was still in flight.
  const inflightKey = accountId + '\n' + t;
  if (_gameNameInflight.has(inflightKey)) return;
  _gameNameInflight.add(inflightKey);
  let name = null;
  try {
    // Fetch via main process (authenticated with cookie)
    name = await api.getGameName(placeId || t, cookie);
  } catch {} finally { _gameNameInflight.delete(inflightKey); }
  // Persist only genuine resolved names (not the raw fallback label).
  if (name) { _gameNamePersist[t] = name; _saveGameNames(); }
  // The target may have been edited (or the account removed) while this was
  // in flight; writing the old game's name would then mislabel the card.
  const cur = accounts.find(a => a.id === accountId);
  if (!cur || (cur.gameTarget || '').trim() !== t) return;
  _gameNameCache[accountId] = name || extractTargetLabel(target);
  updateGameLabel(accountId);
}

function updateGameLabel(accountId) {
  const el = document.getElementById('gt-' + accountId);
  if (!el) return;
  const a = accounts.find(x => x.id === accountId);
  if (!a || !a.gameTarget) return;
  el.textContent = truncate(_gameNameCache[accountId] || extractTargetLabel(a.gameTarget), 22);
}

function truncate(s, n) { return s.length > n ? s.slice(0, n) + '\u2026' : s; }

let _dragSaveTimer = null;
let _dragging = null, _dragClone = null, _dragOffX = 0, _dragOffY = 0, _dragOverId = null;

// Wires up the given cards only: cards kept from the last render already have it.
function initDrag(cards) {
  const grid = document.getElementById('grid');

  cards.forEach(card => {
    const handle = card.querySelector('.drag-handle');
    const startEl = handle || card;

    startEl.addEventListener('mousedown', e => {
      if (e.button !== 0) return;
      if (e.target.closest('button,input')) return;
      e.preventDefault();

      _dragging = card;
      const rect = card.getBoundingClientRect();
      _dragOffX = e.clientX - rect.left;
      _dragOffY = e.clientY - rect.top;

      // Create floating clone
      _dragClone = card.cloneNode(true);
      _dragClone.querySelectorAll('.card-kill, .drag-handle').forEach(el => el.remove());
      _dragClone.style.cssText = `
        position:fixed;left:${rect.left}px;top:${rect.top}px;
        width:${rect.width}px;height:${rect.height}px;
        opacity:0.85;pointer-events:none;z-index:9999;
        box-shadow:0 16px 40px rgba(0,0,0,.6);
        transform:scale(1.04);border-color:var(--ac);
        transition:box-shadow .15s;border-radius:var(--r);
        background:var(--s2);border:1px solid var(--ac);
      `;
      if (grid.classList.contains('list-view')) _dragClone.classList.add('drag-list-clone');
      document.body.appendChild(_dragClone);
      card.style.opacity = '0.3';

      document.addEventListener('mousemove', onDragMove);
      document.addEventListener('mouseup', onDragEnd);
    });
  });
}

function onDragMove(e) {
  if (!_dragging || !_dragClone || !_dragging.isConnected) return;
  _dragClone.style.left = (e.clientX - _dragOffX) + 'px';
  _dragClone.style.top  = (e.clientY - _dragOffY) + 'px';

  // nudge the scroll when the cursor gets near the top/bottom edge
  const wrap = document.querySelector('.grid-wrap');
  if (wrap) {
    const wr = wrap.getBoundingClientRect();
    if (e.clientY < wr.top + 60) wrap.scrollTop -= 16;
    else if (e.clientY > wr.bottom - 60) wrap.scrollTop += 16;
  }

  // Find the card under the cursor (the clone is hidden for the hit-test so it
  // never matches itself).
  _dragClone.style.display = 'none';
  const el = document.elementFromPoint(e.clientX, e.clientY);
  _dragClone.style.display = '';
  const target = el ? el.closest('.card[data-id]') : null;
  if (!target || target === _dragging) return;
  const newId = target.dataset.id;
  if (newId === _dragOverId) return; // already settled against this neighbour
  _dragOverId = newId;

  // Move the dragged node in place instead of a full re-render.
  const grid = document.getElementById('grid');
  const cards = Array.from(grid.querySelectorAll('.card[data-id]'));
  const srcPos = cards.indexOf(_dragging);
  const tgtPos = cards.indexOf(target);
  if (srcPos < 0 || tgtPos < 0) return;
  grid.insertBefore(_dragging, srcPos < tgtPos ? target.nextSibling : target);
  _syncAccountsOrderFromDom();
}

// Reorders `accounts` to match the on-screen card order; accounts hidden by
// an active search/filter keep their positions.
function _syncAccountsOrderFromDom() {
  const grid = document.getElementById('grid');
  const visIds = Array.from(grid.querySelectorAll('.card[data-id]')).map(c => c.dataset.id);
  const visSet = new Set(visIds);
  const byId = new Map(accounts.filter(a => visSet.has(a.id)).map(a => [a.id, a]));
  const queue = visIds.map(id => byId.get(id)).filter(Boolean);
  let qi = 0;
  accounts = accounts.map(a => (visSet.has(a.id) ? queue[qi++] : a));
}

function onDragEnd() {
  document.removeEventListener('mousemove', onDragMove);
  document.removeEventListener('mouseup', onDragEnd);

  if (_dragClone) { _dragClone.remove(); _dragClone = null; }
  if (_dragging) { _dragging.style.opacity = ''; _dragging = null; }
  _dragOverId = null;

  // settle the DOM and rebind the drag handlers with one render
  render();

  clearTimeout(_dragSaveTimer);
  _dragSaveTimer = setTimeout(() => {
    api.reorderAccounts(accounts.map(a => a.id));
  }, 400);
}

function loadAvatar(id, uid) {
  if (_avatarCache[uid]) {
    paintAccountAvatar(id, uid, _avatarCache[uid]);
    return;
  }
  api.robloxGet('https://thumbnails.roblox.com/v1/users/avatar-headshot?userIds=' + uid + '&size=150x150&format=Png')
    .then(res => res.data).then(d => {
      const url = d?.data?.[0]?.imageUrl;
      if (url) {
        _avatarCache[uid] = url;
        paintAccountAvatar(id, uid, url);
      }
    }).catch(() => {});
}

function paintAccountAvatar(id, uid, url) {
  document.getElementById('av-' + id)?.classList.remove('av-loading');
  ['av-' + id, 'pkg-picker-av-' + id, 'tracking-av-' + id].forEach(elId => {
    const el = document.getElementById(elId);
    if (el && !el.querySelector('img')) el.innerHTML = '<img src="' + esc(url) + '" alt=""/>';
  });
}

// One batched request for uncached accounts instead of one per account.
async function loadAvatarsBatch(list) {
  const paint = a => {
    if (a.userId && _avatarCache[a.userId]) {
      paintAccountAvatar(a.id, a.userId, _avatarCache[a.userId]);
    }
  };
  const need = [], seen = new Set();
  for (const a of list) {
    if (!a.userId) continue;
    if (_avatarCache[a.userId]) { paint(a); continue; }
    if (!seen.has(a.userId)) { seen.add(a.userId); need.push(a.userId); }
  }
  for (let i = 0; i < need.length; i += 100) {
    const chunk = need.slice(i, i + 100);
    try {
      const res = await api.robloxGet('https://thumbnails.roblox.com/v1/users/avatar-headshot?userIds=' + chunk.join(',') + '&size=150x150&format=Png');
      const d = res.data;
      (d?.data || []).forEach(item => { if (item && item.targetId && item.imageUrl) _avatarCache[item.targetId] = item.imageUrl; });
      list.forEach(paint);
    } catch {
      chunk.forEach(uid => { const a = list.find(x => x.userId === uid); if (a) loadAvatar(a.id, uid); });
    }
  }
}

function loadPkgAvatar(pkgId, accountId, uid, attempt) {
  const elId = 'pkg-av-' + pkgId + '-' + accountId;
  const paint = url => {
    _avatarCache[uid] = url;
    const el = document.getElementById(elId);
    if (el) el.innerHTML = '<img src="' + esc(url) + '" alt=""/><span class="pkg-avatar-dot"></span>';
  };
  if (_avatarCache[uid]) { paint(_avatarCache[uid]); return; }
  api.robloxGet('https://thumbnails.roblox.com/v1/users/avatar-headshot?userIds=' + uid + '&size=150x150&format=Png')
    .then(res => res.data).then(d => {
      const item = d?.data?.[0];
      if (item && item.imageUrl && item.state === 'Completed') { paint(item.imageUrl); return; }
      // Roblox returns Pending while it generates the thumbnail; retry briefly.
      if (item && item.state === 'Pending' && (attempt || 0) < 3) {
        setTimeout(() => loadPkgAvatar(pkgId, accountId, uid, (attempt || 0) + 1), 1500);
      } else if (item && item.imageUrl) { paint(item.imageUrl); }
    }).catch(() => {});
}

// ── Avatar hover card ───────────────────────────────────────────────────────
const _userInfoCache = {};
function loadUserInfo(uid, cb) {
  if (_userInfoCache[uid]) { cb(_userInfoCache[uid]); return; }
  api.robloxGet('https://users.roblox.com/v1/users/' + uid)
    .then(res => {
      // A 429/404 comes back as { errors: [...] }; caching that pinned the
      // tooltip to "Unknown" for this user for the rest of the session.
      const d = res && res.ok ? res.data : null;
      if (d && d.created) _userInfoCache[uid] = d;
      cb(d);
    })
    .catch(() => cb(null));
}

function positionAvTip(av, tip) {
  const rect = av.getBoundingClientRect();
  const tw = tip.offsetWidth, th = tip.offsetHeight;
  let left = rect.left + rect.width / 2 - tw / 2;
  left = Math.max(8, Math.min(left, window.innerWidth - tw - 8));
  let top = rect.top - th - 10;
  if (top < 8) top = rect.bottom + 10;
  tip.style.left = left + 'px';
  tip.style.top = top + 'px';
}

function showAvTip(av) {
  const uid = av.dataset.uid || '';
  const uname = av.dataset.uname || '';
  const nick = av.dataset.nick || '';
  const tip = document.getElementById('av-tip');
  tip.dataset.uid = uid;
  document.getElementById('av-tip-name').textContent = nick && nick !== uname ? nick : (uname || 'Unknown');
  document.getElementById('av-tip-uname').textContent = uname ? '@' + uname : (uid ? 'ID ' + uid : '');
  const avEl = document.getElementById('av-tip-av');
  avEl.innerHTML = _avatarCache[uid] ? '<img src="' + esc(_avatarCache[uid]) + '" alt=""/>' : esc((uname || '?')[0].toUpperCase());
  document.getElementById('av-tip-created').textContent = uid ? 'Loading\u2026' : 'Unknown';
  tip.classList.add('show');
  positionAvTip(av, tip);
  if (uid) {
    loadUserInfo(uid, info => {
      if (tip.dataset.uid !== uid || !tip.classList.contains('show')) return;
      const createdEl = document.getElementById('av-tip-created');
      if (info && info.created) {
        const d = new Date(info.created);
        createdEl.textContent = 'Created ' + d.toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' });
      } else {
        createdEl.textContent = 'Unknown';
      }
      positionAvTip(av, tip);
    });
  }
}

function hideAvTip() {
  document.getElementById('av-tip').classList.remove('show');
}

document.addEventListener('mouseover', e => {
  const av = e.target.closest('.pkg-avatar:not(.more)');
  if (av) showAvTip(av);
});
document.addEventListener('mouseout', e => {
  const av = e.target.closest('.pkg-avatar:not(.more)');
  if (av && !(e.relatedTarget && av.contains(e.relatedTarget))) hideAvTip();
});
window.addEventListener('scroll', hideAvTip, true);

function _showPanel(panel) {
  ['choose','cookie','browser','creds'].forEach(p => {
    document.getElementById('login-panel-' + p).style.display = p === panel ? '' : 'none';
  });
  document.getElementById('btn-cookie-add').style.display = panel === 'cookie' ? '' : 'none';
  document.getElementById('btn-creds-add').style.display = panel === 'creds' ? '' : 'none';
  document.getElementById('btn-login-cancel').style.display = panel === 'browser' ? '' : 'none';
  const tabs = document.getElementById('login-tabs');
  tabs.style.display = panel === 'browser' ? 'none' : '';
  tabs.querySelectorAll('button').forEach(b => b.classList.toggle('active', b.dataset.panel === panel));
  syncSeg(tabs);
  setStatus('login-status', 'hidden', '');
}

function openLogin() {
  document.getElementById('cookie-input').value = '';
  document.getElementById('creds-username').value = '';
  document.getElementById('creds-password').value = '';
  _showPanel('choose');
  openModal('m-login');
}

function showCookiePanel() {
  _showPanel('cookie');
  setTimeout(() => document.getElementById('cookie-input').focus(), 50);
}

function showCredsPanel() {
  _showPanel('creds');
  setTimeout(() => document.getElementById('creds-username').focus(), 50);
}

// Drives the real Roblox login page with the given credentials rather than
// storing them as a login method; the cookie that comes back is minted for
// this machine's IP, which is what keeps it from being invalidated instantly.
async function addByCreds() {
  const username = document.getElementById('creds-username').value.trim();
  const password = document.getElementById('creds-password').value;
  if (!username || !password) {
    setStatus('login-status', 'err', '<span class="material-icons-round">error_outline</span>Enter both a username and a password');
    return;
  }
  const btn = document.getElementById('btn-creds-add');
  btn.disabled = true;
  btn.innerHTML = '<div class="spin"></div>Signing in';
  setStatus('login-status', 'load', '<div class="spin"></div>Signing in…');
  let res;
  try {
    res = await api.loginWithCredentials(username, password);
  } catch (e) {
    res = { success: false, error: (e && e.message) || 'Login failed' };
  }
  btn.disabled = false;
  btn.innerHTML = '<span class="material-icons-round" style="font-size:15px">login</span>Sign In';
  if (!document.getElementById('m-login').classList.contains('open')) return;
  if (!res || !res.success) {
    setStatus('login-status', 'err', '<span class="material-icons-round">error_outline</span>' + esc((res && res.error) || 'Login failed'));
    return;
  }
  await finishLogin(res);
}

function backToChoose() {
  _showPanel('choose');
}

async function startBrowserLogin() {
  _showPanel('browser');
  // Show waiting state by default - only switch to download UI if Chrome needs to be downloaded
  document.getElementById('login-dl').style.display = 'none';
  document.getElementById('login-waiting').style.display = '';
  document.getElementById('dl-bar').style.width = '0%';
  document.getElementById('dl-pct').textContent = '0%';
  let res;
  try { res = await api.openLogin(); } catch (e) { res = { success: false, error: e?.message || String(e) || 'Login failed' }; }
  if (!document.getElementById('m-login').classList.contains('open')) return;
  if (!res || !res.success) {
    if (res && res.error && res.error !== 'Login window closed') {
      _showPanel('choose');
      setStatus('login-status', 'err', '<span class="material-icons-round">error_outline</span>' + esc(res.error));
    } else {
      closeModal('m-login');
    }
    return;
  }
  await finishLogin(res);
}

async function addByCookie() {
  if (!document.getElementById('cookie-input').value.trim()) return;
  const cookie = normalizeCookie(document.getElementById('cookie-input').value);
  if (!cookie || cookie.length < 100) {
    setStatus('login-status', 'err', '<span class="material-icons-round">error_outline</span>Cookie looks too short - make sure you copied the full value');
    return;
  }
  const btn = document.getElementById('btn-cookie-add');
  btn.disabled = true;
  btn.innerHTML = '<div class="spin"></div>Verifying';
  setStatus('login-status', 'load', '<div class="spin"></div>Verifying cookie…');
  // A rejected invoke used to leave the button stuck on "Verifying".
  let res;
  try { res = await api.validateCookie(cookie); } catch (e) { res = { ok: false, reason: e?.message || String(e) }; }
  btn.disabled = false;
  btn.innerHTML = '<span class="material-icons-round" style="font-size:15px">check</span>Add Account';
  if (!res || !res.ok) {
    // reason can carry Roblox's own error text, so it's escaped like any other.
    setStatus('login-status', 'err', '<span class="material-icons-round">error_outline</span>' + esc((res && res.reason) || 'Invalid cookie: make sure you copied the full .ROBLOSECURITY value'));
    return;
  }
  await finishLogin({ success: true, cookie, username: res.username, userId: res.userId });
}

function cancelLogin() {
  closeModal('m-login');
  api.cancelLogin && api.cancelLogin();
}

async function finishLogin(res) {
  setStatus('login-status', 'ok', '<span class="material-icons-round">check_circle</span>Signed in as ' + esc(res.username));
  // accounts_add can fail (e.g. the store couldn't be written); say so instead
  // of sitting on "Signed in as ..." with nothing added.
  let a;
  try { a = await api.addAccount({ username: res.username, userId: res.userId, cookie: res.cookie, gameTarget: '' }); }
  catch (e) { a = null; setStatus('login-status', 'err', '<span class="material-icons-round">error_outline</span>' + esc('Could not save the account: ' + (e?.message || e))); }
  if (!a) return;
  accounts.push(a); render();
  setTimeout(() => {
    closeModal('m-login');
    // toast() escapes its message itself.
    toast('Added ' + res.username, 'ok');
    const grid = document.getElementById('grid');
    if (grid) grid.lastElementChild?.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
  }, 800);
}

function openEdit(id) {
  editAcc = accounts.find(a => a.id === id); if (!editAcc) return;
  editTab('details');
  document.getElementById('edit-head-name').textContent = editAcc.username || 'Unknown';
  document.getElementById('edit-head-meta').textContent =
    (editAcc.username ? '@' + editAcc.username : '') +
    (editAcc.userId ? (editAcc.username ? ' · ' : '') + 'ID ' + editAcc.userId : '');
  // Letter fallback first, then swap in the cached headshot if we have one.
  const av = document.getElementById('edit-av');
  av.innerHTML = esc((editAcc.username || '?')[0].toUpperCase());
  if (editAcc.userId && _avatarCache[editAcc.userId]) {
    av.innerHTML = '<img src="' + esc(_avatarCache[editAcc.userId]) + '" alt=""/>';
  }
  document.getElementById('in-nickname').value = editAcc.nickname || '';
  document.getElementById('in-tag').value = editAcc.tag || '';
  document.getElementById('in-description').value = editAcc.description || '';
  document.getElementById('in-target').value = editAcc.gameTarget || '';
  document.getElementById('in-ramlimit').value = Number(editAcc.ramLimitMb) > 0 ? editAcc.ramLimitMb : '';
  // Reset follow field/status; it's a lookup tool, not a saved value.
  document.getElementById('in-follow').value = '';
  followSetStatus("Finds the server they're in and fills the target above.", '');
  openModal('m-edit');
  setTimeout(() => document.getElementById('in-nickname').focus(), 220);
}

function editTab(tab) {
  ['details', 'launch'].forEach(t => {
    document.getElementById('edit-panel-' + t).style.display = t === tab ? '' : 'none';
  });
  document.querySelectorAll('#edit-tabs button').forEach(b => b.classList.toggle('active', b.dataset.tab === tab));
  syncSeg(document.getElementById('edit-tabs'));
}

function normalizeTag(v) { return String(v || '').trim().slice(0, 12).toUpperCase(); }
function setEditTag(tag) {
  const el = document.getElementById('in-tag');
  el.value = normalizeTag(el.value) === tag ? '' : tag;
}

function followSetStatus(msg, kind) {
  const el = document.getElementById('follow-status');
  if (!el) return;
  el.textContent = msg;
  el.style.color = kind === 'err' ? 'var(--red)' : (kind === 'ok' ? 'var(--green)' : '');
}

// Uses this account's own cookie for the lookup; presence/username APIs both
// need an authenticated session, and the account being edited is the one that
// would do the joining anyway.
async function followUserLookup() {
  const input = document.getElementById('in-follow');
  const btn = document.getElementById('follow-btn');
  const label = btn ? btn.querySelector('span:last-child') : null;
  const username = (input.value || '').trim();
  if (!username) { followSetStatus('Enter a username', 'err'); return; }
  // Read the live record, not the copy taken when the modal opened.
  const acc = editAcc && accounts.find(a => a.id === editAcc.id);
  if (!acc || !acc.cookie) { followSetStatus('This account has no saved cookie', 'err'); return; }

  if (btn) btn.disabled = true;
  if (label) label.textContent = 'Finding';
  followSetStatus('Looking up ' + username + '...', '');
  try {
    const res = await api.followUser(acc.cookie, username);
    // The edit modal may have been closed or reopened on another account
    // while the lookup ran; don't write into a form that isn't this one's.
    if (!editAcc || editAcc.id !== acc.id) return;
    document.getElementById('in-target').value = res.target;
    followSetStatus('Found ' + res.username + ": target set, click Save to keep it.", 'ok');
  } catch (e) {
    followSetStatus(typeof e === 'string' ? e : (e.message || 'Lookup failed'), 'err');
  } finally {
    if (btn) btn.disabled = false;
    if (label) label.textContent = 'Find';
  }
}

async function saveEdit() {
  if (!editAcc) return;
  // Captured up front: editAcc is module-level and is cleared if the account
  // is removed while this save is awaiting the backend.
  const id = editAcc.id;
  const target = document.getElementById('in-target').value.trim();
  const nickname = document.getElementById('in-nickname').value.trim();
  const description = document.getElementById('in-description').value.trim();
  const ramRaw = document.getElementById('in-ramlimit').value.trim();
  let ramLimitMb = ramRaw === '' ? 0 : parseInt(ramRaw, 10);
  if (!Number.isFinite(ramLimitMb) || ramLimitMb < 0) { toast('RAM limit must be a number of MB (or empty)', 'err'); return; }
  if (ramLimitMb > 0 && ramLimitMb < 512) ramLimitMb = 512;
  const ramChanged = (Number(editAcc.ramLimitMb) || 0) !== ramLimitMb;
  let updated;
  try { updated = await api.updateAccount(id, { gameTarget: target, nickname, description, tag: normalizeTag(document.getElementById('in-tag').value) }); }
  catch (e) { toast('Save failed: ' + (e?.message || e), 'err'); return; }
  // accounts_update answers null when the id no longer exists; claiming
  // "Saved" there hid that the edit went nowhere.
  if (!updated) { closeModal('m-edit'); editAcc = null; toast('This account no longer exists', 'err'); return; }
  const idx = accounts.findIndex(a => a.id === id);
  if (idx !== -1) {
    accounts[idx] = updated;
    delete _gameNameCache[id]; // clear stale name
  }
  // Saved through its own command so a running instance gets the new cap
  // immediately instead of on its next launch.
  let savedMsg = 'Saved', ramError = null;
  if (ramChanged) {
    let r = null;
    try { r = await api.setAccountRamLimit(id, ramLimitMb); } catch (e) { r = { ok: false, error: e?.message || String(e) }; }
    // saved:true means the value is on disk even when applying it to the
    // running instance failed, so the local copy must follow either way.
    if (r && (r.ok || r.saved) && idx !== -1) accounts[idx].ramLimitMb = ramLimitMb;
    if (r && r.ok) {
      if (r.applied) savedMsg = r.limitMb ? 'Saved, RAM limit ' + formatMb(r.limitMb) + ' applied to the running instance' : 'Saved, RAM limit removed from the running instance';
    } else {
      savedMsg = null;
      ramError = 'Saved, but the RAM limit was not applied: ' + ((r && r.error) || 'unknown error');
    }
  }
  render();
  if (idx !== -1 && target) fetchGameName(id, target); // fetch new name immediately (deduped against render's)
  closeModal('m-edit');
  // One toast at the end: the legacy toast is a single element, so an
  // earlier one would just be overwritten by this.
  if (ramError) toast(ramError, 'err');
  else toast(savedMsg, 'ok');
}

function confirmAction(message, onConfirm, confirmLabel = 'Delete') {
  document.getElementById('m-confirm-delete-msg').textContent = message;
  const btn = document.getElementById('m-confirm-delete-btn');
  const newBtn = btn.cloneNode(true); // clone to remove old listeners
  newBtn.textContent = confirmLabel;
  btn.parentNode.replaceChild(newBtn, btn);
  newBtn.addEventListener('click', () => { closeModal('m-confirm-delete'); onConfirm(); });
  // All overlays share the same z-index, so equal-z stacking falls back to
  // DOM order; moving this to the end of <body> guarantees it renders on
  // top even when triggered from inside another already-open modal (e.g.
  // "Clear data" on the encryption lock screen), instead of silently
  // rendering underneath it.
  document.body.appendChild(document.getElementById('m-confirm-delete'));
  openModal('m-confirm-delete');
}

async function removeAcc(id) {
  const a = accounts.find(x => x.id === id);
  if (!a) return;
  // Removing the account drops its PID from the backend's tracking, so an
  // instance left running afterwards is orphaned: still open, but no longer
  // something Kill Roblox can see or close. Say so up front and close it as
  // part of the removal rather than leaving a session nothing can reach.
  const wasRunning = _launchedIds.has(id);
  const msg = wasRunning
    ? 'Remove "' + a.username + '"? Its running Roblox instance will be closed. This cannot be undone.'
    : 'Remove "' + a.username + '"? This cannot be undone.';
  confirmAction(msg, async () => {
    if (wasRunning) { try { await api.killOneRoblox(id); } catch {} }
    // accounts_remove can fail; dropping the card anyway made it reappear on
    // the next launch with no word about why.
    try { await api.removeAccount(id); }
    catch (e) { toast('Could not remove ' + a.username + ': ' + (e?.message || e), 'err'); return; }
    accounts = accounts.filter(x => x.id !== id); render();
    // Drop per-account UI bookkeeping so nothing keeps referring to it.
    _launchedIds.delete(id);
    delete _cookieStatus[id];
    delete _cookieCheckedAt[id];
    delete _cookieCheckedWith[id];
    delete _gameNameCache[id];
    // Same for editAcc: saving it would target an id that no longer exists.
    if (editAcc && editAcc.id === id) { editAcc = null; closeModal('m-edit'); }
    if (packages.some(p => (p.accountIds || []).includes(id))) {
      packages.forEach(p => { p.accountIds = (p.accountIds || []).filter(aid => aid !== id); });
      api.savePackages(packages);
      renderPackages();
    }
    forgetTrackingAccount(id);
    toast('Removed ' + a.username, 'err');
  });
}
async function clearAll() {
  if (!accounts.length) return;
  const genCount = _genHistory.length;
  // Same orphaning problem as removeAcc, for every running account at once.
  const runningCount = accounts.filter(a => _launchedIds.has(a.id)).length;
  const msg = 'Remove all ' + accounts.length + ' account' + (accounts.length === 1 ? '' : 's')
    + (genCount ? ' and ' + genCount + ' generated account' + (genCount === 1 ? '' : 's') + ' from the generator history' : '')
    + '? '
    + (runningCount ? runningCount + ' running Roblox instance' + (runningCount === 1 ? '' : 's') + ' will be closed. ' : '')
    + 'This cannot be undone.';
  confirmAction(msg, async () => {
    if (runningCount) { try { await api.killAllRoblox(); } catch {} }
    // One failed remove used to abort the loop mid-way and skip the rest of
    // this handler, leaving the UI out of step with what was actually deleted.
    const kept = [];
    for (const a of accounts) {
      try { await api.removeAccount(a.id); forgetTrackingAccount(a.id); }
      catch { kept.push(a); }
    }
    const keptIds = new Set(kept.map(a => a.id));
    accounts = kept; render(); document.getElementById('stat-count').textContent = String(kept.length);
    _launchedIds.clear();
    packages.forEach(p => { p.accountIds = (p.accountIds || []).filter(aid => keptIds.has(aid)); });
    api.savePackages(packages);
    renderPackages();
    // The generator history holds usernames, passwords and cookies of its own,
    // so "remove all saved accounts and sign-in data" has to take it too;
    // otherwise the most sensitive records survive the wipe.
    if (genCount) {
      _genHistory = [];
      _lastGenData = null;
      genRenderHistory();
      try { await api.clearGenHistory(); } catch {}
    }
    toast(kept.length ? ('Could not remove ' + kept.length + ' account' + (kept.length === 1 ? '' : 's')) : 'All accounts cleared', 'err');
  });
}

// Bumped each time the launch modal is (re)opened, so async work started for
// an earlier opening (avatar fetch, a launch still resolving after the modal
// was dismissed) can tell it no longer owns the modal's contents.
// Accounts with a launch in progress. A launch can take ~30s (stagger, ticket
// retries, spawn retries), so the card shows it and a second click cancels it.
const _launchingIds = new Set();
function setCardLaunching(id, on) {
  if (on) _launchingIds.add(id); else _launchingIds.delete(id);
  document.querySelector(`.card[data-id="${id}"]`)?.classList.toggle('is-launching', on);
}

function cardPrimaryAction(id) {
  if (_launchedIds.has(id)) killOne(id);
  else if (_launchingIds.has(id)) api.cancelLaunch(id).catch(() => {});
  else startAccount(id);
}

// Text typed into each row's follow box, kept so a re-render doesn't wipe it.
const _followDraft = {};

// Finds the server a user is in and launches this account into it, for this
// launch only: the account's saved game target is left alone.
async function followFromRow(id) {
  if (_launchedIds.has(id) || _launchingIds.has(id)) return;
  const acc = accounts.find(a => a.id === id);
  if (!acc) return;
  const username = (_followDraft[id] || '').trim();
  const rowBtn = () => document.querySelector(`.card[data-id="${id}"] .card-follow-btn`);
  if (!username) {
    document.getElementById('follow-' + id)?.focus();
    toast('Enter a username to follow', 'err');
    return;
  }
  if (!acc.cookie) { toast('This account has no saved cookie', 'err'); return; }
  if (rowBtn()) { rowBtn().disabled = true; rowBtn().textContent = 'Finding'; }
  let found;
  try {
    found = await api.followUser(acc.cookie, username);
  } catch (e) {
    toast(typeof e === 'string' ? e : (e?.message || 'Could not find that user'), 'err');
    return;
  } finally {
    if (rowBtn()) { rowBtn().disabled = false; rowBtn().textContent = 'Follow'; }
  }
  startAccount(id, found.target);
}

async function startAccount(id, followTarget) {
  if (_launchingIds.has(id)) return;
  if (_launchedIds.has(id)) { toast('This account is already running', 'err'); return; }
  const acc = accounts.find(a => a.id === id);
  if (!acc) return;
  setCardLaunching(id, true);
  logEntry('info', 'launch', `Launching Roblox for ${acc.username || acc.id}...`, { accountId: acc.id, username: acc.username, userId: acc.userId, target: followTarget || acc.gameTarget || 'Roblox home' });
  let res;
  try {
    res = await api.launchRoblox(acc.id, acc.cookie, followTarget || acc.gameTarget || null, !followTarget);
  } catch (e) {
    res = { success: false, error: e?.message || String(e) };
  }
  setCardLaunching(id, false);
  if (!res) res = { success: false, error: 'Launch failed' };
  if (res.cancelled) { logEntry('warn', 'launch', 'Launch cancelled', { accountId: acc.id }); return; }
  if (!res.success) {
    logEntry('err', 'launch', `Launch failed for ${acc.username || acc.id}: ${res.error}`, { accountId: acc.id });
    _flagCookieMaybeDead(acc.id, res.error);
    toast('Launch failed for ' + (acc.username || acc.id) + ': ' + res.error, 'err');
    return;
  }
  logEntry('ok', 'launch', `Roblox launched successfully as ${acc.username || acc.id}`, { accountId: acc.id, username: acc.username, userId: acc.userId });
  markLaunched(acc.id);
  toast('Launched as ' + acc.username, 'ok');
}

// ── Packages ──────────────────────────────────────────────────────────────
const PKG_SERVER_MODES = {
  same: { label: 'Same server', desc: 'Everyone joins the link above' },
  random: { label: 'Random servers', desc: 'Each account in its own public server' },
  closest: { label: 'Closest servers', desc: 'Own server each, lowest ping first' },
  private: { label: 'Own private servers', desc: "Each account in its own private server" },
};
function setPackageMode(id, mode) {
  const p = packages.find(x => x.id === id);
  if (!p) return;
  p.serverMode = mode;
  api.savePackages(packages);
  closeAllCdd();
  renderPackages();
}

function renderPackages() {
  const list = document.getElementById('pkg-list'), empty = document.getElementById('pkg-empty');
  if (!list) return;
  if (!packages.length) { list.innerHTML = ''; empty.style.display = 'flex'; return; }
  empty.style.display = 'none';
  list.innerHTML = packages.map((p, i) => {
    const members = (p.accountIds || []).map(id => accounts.find(a => a.id === id)).filter(Boolean);
    const shown = members.slice(0, 6);
    const extra = members.length - shown.length;
    const avatarsHtml = shown.map(m => `<div class="pkg-avatar${_launchedIds.has(m.id) ? ' online' : ''}" id="pkg-av-${p.id}-${m.id}" data-acc-id="${m.id}" data-uid="${esc(m.userId || '')}" data-uname="${esc(m.username || '')}" data-nick="${esc(m.nickname || '')}">${esc((m.username || '?')[0].toUpperCase())}<span class="pkg-avatar-dot"></span></div>`).join('')
      + (extra > 0 ? `<div class="pkg-avatar more">+${extra}</div>` : '');
    return `
    <div class="pkg-card" data-id="${p.id}" style="animation-delay:${i * 18}ms">
      <div class="pkg-card-top">
        <div class="sr-icon"><span class="material-icons-round">groups</span></div>
        <div class="pkg-card-info">
          <div class="pkg-name">${esc(p.name)}</div>
          <div class="pkg-meta"></div>
          <div class="pkg-run-bar"><span></span></div>
        </div>
        <div class="pkg-avatars">${avatarsHtml}</div>
        <div class="pkg-card-actions">
          <button class="btn btn-edit" onclick="openEditPackage(${jsArg(p.id)})" title="Manage accounts" aria-label="Manage accounts">
            <span class="material-icons-round">group</span>
          </button>
          <button class="btn btn-del" onclick="deletePackage(${jsArg(p.id)})" title="Delete group" aria-label="Delete group">
            <span class="material-icons-round">delete_outline</span>
          </button>
        </div>
      </div>
      <div class="pkg-link-row">
        <div class="pkg-link-field">
          <span class="material-icons-round pkg-link-icon">link</span>
          <input type="text" class="pkg-link-input" id="pkg-link-${p.id}" placeholder="Game ID or server link for everyone to join…"
            value="${esc(p.link || '')}" onchange="setPackageLink(${jsArg(p.id)}, this.value)"
            onkeydown="if(event.key==='Enter'){this.blur();launchPackage(${jsArg(p.id)});}"/>
        </div>
        <div class="cdd pkg-mode" id="cdd-pkgmode-${p.id}">
          <div class="cdd-trigger" id="cdd-pkgmode-${p.id}-trigger" onclick="toggleCdd('pkgmode-${p.id}')" role="button" tabindex="0" aria-label="Where accounts join">
            <span class="cdd-label">${esc(PKG_SERVER_MODES[p.serverMode || 'same'].label)}</span>
            <span class="material-icons-round cdd-arrow">expand_more</span>
          </div>
          <div class="cdd-menu" id="cdd-pkgmode-${p.id}-menu">
            ${Object.entries(PKG_SERVER_MODES).map(([k, m]) => `<div class="cdd-option${(p.serverMode || 'same') === k ? ' selected' : ''}" onclick="setPackageMode(${jsArg(p.id)},'${k}')" role="button" tabindex="0"><div class="cdd-opt-left"><div class="cdd-opt-name">${m.label}</div><div class="cdd-opt-desc">${esc(m.desc)}</div></div></div>`).join('')}
          </div>
        </div>
        <button class="btn btn-launch pkg-launch-btn" onclick="launchPackage(${jsArg(p.id)})" ${members.length ? '' : 'disabled'}>
          Start All
        </button>
        <button class="btn btn-del pkg-kill-btn" onclick="killPackage(${jsArg(p.id)})" title="Stop this group's accounts" aria-label="Stop this group's accounts" ${members.some(m => _launchedIds.has(m.id)) ? '' : 'disabled'}>
          <span class="material-icons-round">power_settings_new</span>
        </button>
      </div>
      <div class="pkg-progress" id="pkg-progress-${p.id}"></div>
    </div>`;
  }).join('');
  packages.forEach(p => {
    (p.accountIds || []).slice(0, 6).forEach(id => {
      const m = accounts.find(a => a.id === id);
      if (m && m.userId) loadPkgAvatar(p.id, m.id, m.userId);
    });
  });
  refreshPkgAvatarStatus();
}

function openCreatePackage() {
  editingPackageId = null;
  document.getElementById('pkg-modal-title').textContent = 'New group';
  document.getElementById('in-pkg-name').value = '';
  renderPackagePicker([]);
  openModal('m-package');
  setTimeout(() => document.getElementById('in-pkg-name').focus(), 220);
}

function openEditPackage(id) {
  const p = packages.find(x => x.id === id); if (!p) return;
  editingPackageId = id;
  document.getElementById('pkg-modal-title').textContent = 'Edit group';
  document.getElementById('in-pkg-name').value = p.name || '';
  renderPackagePicker(p.accountIds || []);
  openModal('m-package');
}

function renderPackagePicker(selectedIds) {
  const wrap = document.getElementById('pkg-account-picker');
  if (!accounts.length) {
    wrap.innerHTML = '<div class="pkg-pick-empty">No accounts yet. Add one from the Accounts tab first.</div>';
    updatePkgCount();
    return;
  }
  wrap.innerHTML = accounts.map(a => `
    <label class="pm-row">
      <input type="checkbox" value="${a.id}" ${selectedIds.includes(a.id) ? 'checked' : ''}/>
      <span class="pm-av" id="pkg-picker-av-${a.id}">${esc((a.username || '?')[0].toUpperCase())}</span>
      <span class="pm-info">
        <span class="pm-name">${esc(a.nickname || a.username || 'Unknown')}</span>
        <span class="pm-meta">${a.userId ? 'ID ' + esc(a.userId) : 'No ID'}</span>
      </span>
      <span class="pm-check"><span class="material-icons-round">check</span></span>
    </label>`).join('');
  updatePkgCount();
  loadAvatarsBatch(accounts);
}

function updatePkgCount() {
  const el = document.getElementById('pkg-count');
  if (!el) return;
  const n = document.querySelectorAll('#pkg-account-picker input:checked').length;
  el.textContent = n + ' selected';
}

function savePackageModal() {
  const name = document.getElementById('in-pkg-name').value.trim();
  if (!name) { toast('Give the group a name', 'err'); return; }
  const checked = Array.from(document.querySelectorAll('#pkg-account-picker input:checked')).map(c => c.value);
  if (editingPackageId) {
    const p = packages.find(x => x.id === editingPackageId);
    if (p) { p.name = name; p.accountIds = checked; }
  } else {
    packages.push({ id: crypto.randomUUID(), name, accountIds: checked, link: '' });
  }
  api.savePackages(packages);
  renderPackages();
  closeModal('m-package');
  toast('Group saved', 'ok');
}

function deletePackage(id) {
  const p = packages.find(x => x.id === id); if (!p) return;
  confirmAction('Delete package "' + p.name + '"? The accounts themselves won\u2019t be removed.', () => {
    packages = packages.filter(x => x.id !== id);
    api.savePackages(packages);
    renderPackages();
    toast('Group deleted', 'err');
  });
}

function setPackageLink(id, value) {
  const p = packages.find(x => x.id === id); if (!p) return;
  p.link = value.trim();
  api.savePackages(packages);
}

async function launchPackage(id) {
  const p = packages.find(x => x.id === id); if (!p) return;
  const members = (p.accountIds || []).map(aid => accounts.find(a => a.id === aid)).filter(Boolean);
  if (!members.length) { toast('This group has no accounts yet', 'err'); return; }

  const card = document.querySelector('.pkg-card[data-id="' + id + '"]');
  const btn = card ? card.querySelector('.pkg-launch-btn') : null;
  const killBtn = card ? card.querySelector('.pkg-kill-btn') : null;
  // launchPackage/killPackage share the same progress chips; block one while the other runs.
  if (btn && btn.dataset.busy === '1') return;
  if (killBtn && killBtn.dataset.busy === '1') { toast('A kill is already in progress for this group', 'err'); return; }
  if (btn) btn.dataset.busy = '1';
  if (killBtn) killBtn.disabled = true;
  const progress = document.getElementById('pkg-progress-' + id);
  if (btn) { btn.disabled = true; btn.innerHTML = '<div class="spin"></div>Launching'; }
  if (progress) {
    progress.innerHTML = members.map(m => `
      <span class="pkg-chip load" id="pkg-chip-${id}-${m.id}">
        <div class="spin" style="width:9px;height:9px;border-width:2px"></div>${esc(m.nickname || m.username || '')}
      </span>`).join('');
  }

  const link = (p.link || '').trim();
  let okCount = 0, skipped = 0, targetsChanged = false, firstError = null;
  // A second client on the same account can get the first one disconnected,
  // and the app would lose track of it, so Start All only starts what's stopped.
  const markSkipped = (m) => {
    skipped++;
    const chip = document.getElementById('pkg-chip-' + id + '-' + m.id);
    if (chip) {
      chip.className = 'pkg-chip ok';
      chip.title = 'Already running';
      chip.innerHTML = '<span class="material-icons-round">check_circle</span>' + esc(m.nickname || m.username || '');
    }
  };
  // "Different servers" modes get one target per account, chosen up front.
  const serverMode = p.serverMode || 'same';
  let plan = null, planError = null;
  if (serverMode !== 'same') {
    try {
      if (!link) throw new Error('Add a game ID or link to this group first');
      plan = await api.serversPlan(link, serverMode, members.filter(m => !_launchedIds.has(m.id)).map(m => m.id));
    } catch (e) { planError = typeof e === 'string' ? e : (e.message || 'Could not pick servers'); }
  }
  await Promise.all(members.map(async (m) => {
    if (_launchedIds.has(m.id)) { markSkipped(m); return; }
    const assigned = plan ? plan[m.id] : null;
    const target = serverMode === 'same' ? (link || m.gameTarget || null) : (assigned && assigned.target) || null;
    const planFailure = serverMode !== 'same' && !target ? (planError || (assigned && assigned.error) || 'No server found') : null;
    logEntry('info', 'launch', `Launching Roblox for ${m.username || m.id} (package)...`, { accountId: m.id, username: m.username || null, userId: m.userId || null, target: target || 'Roblox home' });
    // One rejected invoke used to reject the whole Promise.all, skipping the
    // reset below and leaving "Start All" disabled and busy for good.
    let res;
    if (planFailure) res = { success: false, error: planFailure };
    else try { res = await api.launchRoblox(m.id, m.cookie, target, serverMode === 'same'); } catch (e) { res = { success: false, error: e?.message || String(e) }; }
    if (!res) res = { success: false, error: 'Launch failed' };
    if (res.alreadyRunning) { markSkipped(m); return; }
    const chip = document.getElementById('pkg-chip-' + id + '-' + m.id);
    if (res.success) {
      okCount++;
      // The backend stores the target it launched into as the account's
      // gameTarget; mirror that, or the card keeps the old target and the
      // next Edit > Save writes the stale value back over it.
      if (serverMode === 'same' && target && (m.gameTarget || '') !== target) {
        const live = accounts.find(a => a.id === m.id);
        if (live) { live.gameTarget = target; delete _gameNameCache[m.id]; targetsChanged = true; }
      }
      logEntry('ok', 'launch', `Launched as ${m.username || m.id} (package)`, { accountId: m.id, username: m.username || null });
      markLaunched(m.id);
      if (chip) { chip.className = 'pkg-chip ok'; chip.innerHTML = '<span class="material-icons-round">check_circle</span>' + esc(m.nickname || m.username || ''); }
    } else {
      // The reason used to live only in the chip's hover title, so a failed
      // group launch just said "0/2" with nothing to act on.
      if (!res.cancelled) {
        if (!firstError) firstError = res.error || 'Launch failed';
        logEntry('err', 'launch', `Launch failed for ${m.username || m.id} (package): ${res.error || 'unknown error'}`, { accountId: m.id });
        _flagCookieMaybeDead(m.id, res.error);
      }
      if (chip) {
        chip.className = 'pkg-chip err';
        chip.title = res.error || '';
        chip.innerHTML = '<span class="material-icons-round">error_outline</span>' + esc(m.nickname || m.username || '');
      }
    }
  }));

  if (targetsChanged) render();
  if (btn) { btn.disabled = false; btn.innerHTML = 'Start All'; delete btn.dataset.busy; }
  if (killBtn && killBtn.dataset.busy !== '1') killBtn.disabled = !members.some(m => _launchedIds.has(m.id));
  const wanted = members.length - skipped;
  if (!wanted) { toast('All accounts in "' + p.name + '" are already running', 'ok'); return; }
  toast('Launched ' + okCount + '/' + wanted + ' accounts in "' + p.name + '"' + (skipped ? ' (' + skipped + ' already running)' : '') + (firstError ? ': ' + firstError : ''), okCount === wanted ? 'ok' : 'err');
}

async function killPackage(id) {
  const p = packages.find(x => x.id === id); if (!p) return;
  const members = (p.accountIds || []).map(aid => accounts.find(a => a.id === aid)).filter(Boolean);
  const running = members.filter(m => _launchedIds.has(m.id));
  if (!running.length) { toast('No running instances in this group', 'err'); return; }

  const card = document.querySelector('.pkg-card[data-id="' + id + '"]');
  const btn = card ? card.querySelector('.pkg-kill-btn') : null;
  const launchBtn = card ? card.querySelector('.pkg-launch-btn') : null;
  if (btn && btn.dataset.busy === '1') return;
  if (launchBtn && launchBtn.dataset.busy === '1') { toast('A launch is already in progress for this group', 'err'); return; }
  if (btn) btn.dataset.busy = '1';
  if (launchBtn) launchBtn.disabled = true;
  const progress = document.getElementById('pkg-progress-' + id);
  if (btn) btn.disabled = true;
  if (progress) {
    progress.innerHTML = running.map(m => `
      <span class="pkg-chip load" id="pkg-chip-${id}-${m.id}">
        <div class="spin" style="width:9px;height:9px;border-width:2px"></div>${esc(m.nickname || m.username || '')}
      </span>`).join('');
  }

  let okCount = 0;
  await Promise.all(running.map(async (m) => {
    logEntry('warn', 'kill', `Killing Roblox instance for ${m.username || m.id} (package)...`, { accountId: m.id, username: m.username || null, userId: m.userId || null });
    // Same as launchPackage: a rejection must not strand the busy buttons.
    let res;
    try { res = await api.killOneRoblox(m.id); } catch (e) { res = { ok: false, error: e?.message || String(e) }; }
    const chip = document.getElementById('pkg-chip-' + id + '-' + m.id);
    if (res && res.ok) {
      okCount++;
      logEntry('ok', 'kill', `Killed instance for ${m.username || m.id} (package)`, { accountId: m.id });
      if (chip) { chip.className = 'pkg-chip ok'; chip.innerHTML = '<span class="material-icons-round">check_circle</span>' + esc(m.nickname || m.username || ''); }
    } else if (chip) {
      chip.className = 'pkg-chip err';
      chip.title = (res && res.error) || '';
      chip.innerHTML = '<span class="material-icons-round">error_outline</span>' + esc(m.nickname || m.username || '');
    }
  }));

  if (btn) { btn.disabled = !members.some(m => _launchedIds.has(m.id)); delete btn.dataset.busy; }
  if (launchBtn && launchBtn.dataset.busy !== '1') launchBtn.disabled = false;
  toast('Killed ' + okCount + '/' + running.length + ' instances in "' + p.name + '"', okCount === running.length ? 'ok' : 'err');
}

function toggleKeyVisibility(inputId = 'custom-key', iconId = 'key-vis-icon') {
  const input = document.getElementById(inputId), icon = document.getElementById(iconId);
  if (input.type === 'password') { input.type = 'text'; icon.textContent = 'visibility_off'; }
  else { input.type = 'password'; icon.textContent = 'visibility'; }
}
let _saveKeyTimer;
function onKeyInput() {
  const btn = document.getElementById('btn-save-key');
  if (btn) { btn.disabled = false; btn.textContent = 'Save'; }
}
async function saveKeySettings() {
  const keyVal = document.getElementById('custom-key').value;
  const btn = document.getElementById('btn-save-key');
  if (btn.disabled) return;
  clearTimeout(_saveKeyTimer);
  btn.disabled = true; btn.textContent = 'Saving';
  _saveKeyTimer = setTimeout(async () => {
    try {
      // Checked before the call, not after it: enc_set_key treats an empty
      // (or whitespace-only) value as "remove the passphrase", so checking
      // afterwards reported a failure after the key had already been dropped
      // and every account re-encrypted under DPAPI.
      if (!keyVal.trim()) throw new Error('Encryption key cannot be empty.');
      // enc:setKey changes the key and re-encrypts accounts in one step.
      const r = await api.encSetKey(keyVal);
      if (!r || !r.ok) throw new Error(r && r.error ? r.error : 'could not update key');
      settings.keySet = true;
      document.getElementById('custom-key').value = '';
      // Reload accounts so the renderer holds cookies under the new key.
      try { accounts = await api.loadAccounts(); flagInvalidCookies(accounts); render(); } catch {}
      toast('Encryption key updated', 'ok');
      applySettings();
    } catch (e) {
      // invoke rejects with a plain string, which has no .message.
      toast('Save failed: ' + (e?.message || e), 'err');
    } finally {
      btn.disabled = false; btn.textContent = 'Save';
    }
  }, 300);
}

function openModal(id) { document.getElementById(id).classList.add('open'); }
function closeModal(id) { document.getElementById(id).classList.remove('open'); }
function setStatus(id, type, html) { const el = document.getElementById(id); el.className = 'mst ' + type; el.innerHTML = html; }
function esc(s) { return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;').replace(/'/g,'&#39;'); }
// A string argument for an inline on*="..." handler. esc() alone isn't enough
// there: the browser decodes &#39; back to ' before the JS is parsed, so a
// quote in the value would still end the string literal. JSON-encode first
// (a real JS string literal), then esc() it for the double-quoted attribute.
function jsArg(s) { return esc(JSON.stringify(String(s))); }
function toast(msg, type) {
  type = type || '';
  const el = document.getElementById('toast'), icon = type === 'ok' ? 'check_circle' : 'cancel';
  el.innerHTML = '<span class="material-icons-round">' + icon + '</span>' + esc(msg);
  el.className = 'toast show ' + type; clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.remove('show'), 2700);
}

// Defaults multi-instance on for a fresh install, but leaves an explicit
// `false` alone; this used to force it back on every time the Settings page
// was opened, so the setting could never stay off.
async function refreshMultiStatus() {
  const s = await api.multiInstanceStatus();
  // Driven off the backend's own view rather than the local settings copy, so
  // the switch can't sit out of sync with what the helper is actually doing.
  // That side defaults to on now, so a fresh install shows the switch already
  // enabled without needing the setting written out first.
  const cb = document.getElementById('set-multiinstance');
  if (cb) cb.checked = !!s.enabled;
}

document.querySelectorAll('.overlay').forEach(o => {
  o.addEventListener('mousedown', e => {
    if (e.target === o && o.dataset.backdropClose === 'true') o.classList.remove('open');
  });
});
document.addEventListener('keydown', e => {
  // Ctrl/Cmd+F opens native-style find on the logs page.
  if ((e.ctrlKey || e.metaKey) && (e.key === 'f' || e.key === 'F') && document.getElementById('page-logs')?.classList.contains('active')) {
    e.preventDefault(); openLogFind(); return;
  }
  // Find-bar keys: Enter = next, Shift+Enter = previous, Esc = close.
  if (e.target && e.target.id === 'log-find-input') {
    if (e.key === 'Enter') { e.preventDefault(); logFind(e.shiftKey); return; }
    if (e.key === 'Escape') { e.preventDefault(); closeLogFind(); return; }
  }
  if (e.key === 'Escape') {
    const lf = document.getElementById('log-find');
    if (lf && lf.style.display !== 'none') { closeLogFind(); return; }
    closeAllCdd();
    const editEl = document.getElementById('m-edit');
    if (editEl.classList.contains('open')) {
      // Escape shouldn't discard the modal out from under someone mid-typing.
      const typing = ['in-target', 'in-nickname', 'in-description', 'in-follow', 'in-ramlimit']
        .some(f => document.activeElement === document.getElementById(f));
      if (!typing) closeModal('m-edit');
    // The encryption gate is not dismissable (data-backdrop-close="false"):
    // escaping it skipped continueInit and left an empty, dead window.
    } else document.querySelectorAll('.overlay.open').forEach(m => { if (m.id !== 'm-enc') m.classList.remove('open'); });
  }
  // Same gate: no adding accounts before storage has been unlocked.
  if ((e.ctrlKey || e.metaKey) && e.key === 'n') { e.preventDefault(); if (!document.getElementById('m-enc').classList.contains('open')) openLogin(); }
  // "/" focuses the account search (when not already typing in a field).
  if (e.key === '/' && document.getElementById('page-accounts')?.classList.contains('active')
      && !/^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement?.tagName || '')) {
    e.preventDefault();
    document.getElementById('acct-search')?.focus();
  }
  if ((e.ctrlKey || e.metaKey) && e.key === 's' && document.getElementById('m-edit').classList.contains('open')) {
    e.preventDefault(); saveEdit();
  }
});

let chartTab = 'popular';
let allCharts = {};
let chartsLoaded = false;

// ── Charts memory management ──────────────────────────────────────────────
// Charts holds the heaviest payload in the app: three sorts of ~50 games each,
// every card carrying a 512x512 PNG. Decoded, that is tens of MB of bitmap the
// browser keeps resident for a page the user may have looked at once. Two
// levels of unloading, both on the same 10s delay so a quick scroll-back or a
// tab bounce never pays to re-fetch:
//
//   1. Per image: a card scrolled out of the list drops its src, and reloads
//      it when it scrolls back. The URL lives on data-src so nothing is lost.
//   2. Whole page: leaving the Charts tab drops the game data and the grid DOM
//      entirely, so returning re-fetches fresh (which is what you want from a
//      "top playing now" list anyway).
const CHART_UNLOAD_MS = 10000;
let _chartObserver = null;
const _chartImgTimers = new Map(); // img element -> pending unload timeout
let _chartsIdleTimer = null;

function _chartLoadImg(img) {
  const src = img.dataset.src;
  if (src && img.getAttribute('src') !== src) img.setAttribute('src', src);
}

function _chartUnloadImg(img) {
  if (!img.hasAttribute('src')) return;
  // Removing src can raise the element's error event; flag it so the handler
  // can tell our own unload from a genuinely broken image and doesn't swap in
  // the "no thumbnail" placeholder permanently.
  img.dataset.unloading = '1';
  img.removeAttribute('src');
  setTimeout(() => { delete img.dataset.unloading; }, 0);
}

function _chartThumbError(img) {
  if (img.dataset.unloading === '1') return;
  img.outerHTML = '<div class="chart-card-thumb-ph"><span class="material-icons-round">videogame_asset</span></div>';
}

function _chartClearImgTimers() {
  _chartImgTimers.forEach(t => clearTimeout(t));
  _chartImgTimers.clear();
}

/// Rebuilt on every render, since the previous grid's elements are gone.
function _chartObserveImages() {
  if (_chartObserver) _chartObserver.disconnect();
  _chartClearImgTimers();
  const grid = document.getElementById('charts-grid');
  if (!grid || typeof IntersectionObserver === 'undefined') {
    // No observer support: just load everything, same as the old behaviour.
    if (grid) grid.querySelectorAll('img.chart-card-thumb').forEach(_chartLoadImg);
    return;
  }
  _chartObserver = new IntersectionObserver((entries) => {
    for (const e of entries) {
      const img = e.target;
      const pending = _chartImgTimers.get(img);
      if (e.isIntersecting) {
        if (pending) { clearTimeout(pending); _chartImgTimers.delete(img); }
        _chartLoadImg(img);
      } else if (!pending) {
        _chartImgTimers.set(img, setTimeout(() => {
          _chartImgTimers.delete(img);
          _chartUnloadImg(img);
        }, CHART_UNLOAD_MS));
      }
    }
  }, {
    root: document.querySelector('.charts-scroll'),
    // Start fetching just before a card scrolls in, so it is rarely blank.
    rootMargin: '250px 0px',
  });
  grid.querySelectorAll('img.chart-card-thumb').forEach(img => _chartObserver.observe(img));
}

/// Drops everything Charts holds. Safe to call when already unloaded.
function unloadCharts() {
  _chartsLoadSeq++; // strand any fetch still in flight
  if (_chartObserver) { _chartObserver.disconnect(); _chartObserver = null; }
  _chartClearImgTimers();
  const grid = document.getElementById('charts-grid');
  if (grid) { grid.innerHTML = ''; grid.style.display = 'none'; }
  const empty = document.getElementById('charts-empty');
  if (empty) empty.style.display = 'none';
  allCharts = {};
  _chartGameMap = {};
  chartsLoaded = false;
}

function chartsOnEnter() {
  clearTimeout(_chartsIdleTimer);
  _chartsIdleTimer = null;
}

function chartsOnLeave() {
  clearTimeout(_chartsIdleTimer);
  _chartsIdleTimer = setTimeout(() => {
    _chartsIdleTimer = null;
    unloadCharts();
  }, CHART_UNLOAD_MS);
}

function switchChartTab(tab) {
  chartTab = tab;
  document.querySelectorAll('#page-charts .tab-btn').forEach(t => t.classList.remove('active'));
  document.getElementById('ctab-' + tab).classList.add('active');
  const s = document.getElementById('chart-search'); if (s) s.value = '';
  _searchMode = false;
  if (chartsLoaded) renderCharts(allCharts[tab] || [], false);
  positionTabSlider(document.getElementById('ctab-' + tab).closest('.tab-bar'));
}

// Bumped on every load and by unloadCharts, so a fetch that finishes after the
// user has navigated away (or hit Refresh again) is discarded instead of
// repopulating the data we just dropped.
let _chartsLoadSeq = 0;

document.getElementById('charts-loading').innerHTML = '<div class="charts-grid">'
  + '<div class="chart-feature chart-skel"><div class="skel chart-feature-media"></div><div class="chart-feature-info"><div class="skel skel-line" style="width:30%;margin-bottom:14px"></div><div class="skel skel-line" style="height:22px;width:70%;margin-bottom:14px"></div><div class="skel skel-line short"></div></div></div>'
  + '<div class="chart-card chart-skel"><div class="skel chart-skel-thumb"></div><div class="skel skel-line"></div><div class="skel skel-line short"></div></div>'.repeat(14)
  + '</div>';

async function loadCharts() {
  const grid = document.getElementById('charts-grid');
  const loading = document.getElementById('charts-loading');
  const empty = document.getElementById('charts-empty');
  chartsLoaded = false;
  const seq = ++_chartsLoadSeq;
  // Landing here means the user is on the tab; cancel any pending unload.
  chartsOnEnter();
  grid.style.display = 'none'; empty.style.display = 'none'; loading.style.display = 'flex';

  try {
    // Use official Roblox explore-api with a random sessionId per load
    const [popular, trending, favorited] = await Promise.all([
      fetchRobloxGames('top-playing-now'),
      fetchRobloxGames('top-rated'),
      fetchRobloxGames('top-earning'),
    ]);
    if (seq !== _chartsLoadSeq) return; // superseded or unloaded while fetching
    allCharts = { popular, trending, favorited };
    chartsLoaded = true;
    loading.style.display = 'none';
    renderCharts(allCharts[chartTab] || [], false);
  } catch(e) {
    if (seq !== _chartsLoadSeq) return;
    console.error('Charts load error:', e);
    loading.style.display = 'none';
    empty.style.display = 'flex';
  }
}

function randomGuid() {
  return 'xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx'.replace(/[xy]/g, c => {
    const r = Math.random() * 16 | 0;
    return (c === 'x' ? r : (r & 0x3 | 0x8)).toString(16);
  });
}

async function fetchRobloxGames(sortId) {
  // Official Roblox explore API
  const sessionId = randomGuid();
  const url = `https://apis.roblox.com/explore-api/v1/get-sort-content?sessionId=${sessionId}&sortId=${sortId}&device=computer&country=all`;
  const r = await api.robloxGet(url);
  if (!r.ok) throw new Error(`HTTP ${r.status}`);
  const d = r.data;

  // Response shape: { sorts: [{ games: [...] }] } or { games: [...] }
  const games = (d.games || (d.sorts && d.sorts[0] && d.sorts[0].games) || []).filter(g => !g.isSponsored);
  if (!games.length) throw new Error('No games in response');

  // Fetch thumbnails for all universeIds
  let thumbMap = {};
  try {
    const universeIds = games.map(g => g.universeId).filter(Boolean).join(',');
    const thumbRes = await api.robloxGet(
      `https://thumbnails.roblox.com/v1/games/icons?universeIds=${universeIds}&returnPolicy=PlaceHolder&size=512x512&format=Png&isCircular=false`
    );
    if (thumbRes.ok) {
      const thumbData = thumbRes.data;
      (thumbData.data || []).forEach(t => { thumbMap[t.targetId] = t.imageUrl; });
    }
  } catch {}

  return games.map(g => ({
    universeId: g.universeId,
    placeId: g.rootPlaceId || g.placeId,
    name: g.name,
    playerCount: g.playerCount,
    likes: likePct(g.totalUpVotes, g.totalDownVotes),
    thumbUrl: thumbMap[g.universeId] || ''
  }));
}

function likePct(up, down) {
  return typeof up === 'number' && typeof down === 'number' && up + down > 0 ? Math.round(up / (up + down) * 100) : null;
}
const cssUrl = u => `url("${String(u).replace(/["\\\n]/g, encodeURIComponent)}")`;

// The top game of a chart gets a wide banner with its own screenshot.
function chartFeatureHtml(g) {
  const chart = document.querySelector('#page-charts .tab-btn.active')?.textContent || 'this chart';
  return `<div class="chart-feature" onclick="openGameModal(0)" title="View game info">
    <canvas class="chart-feature-glow" id="chart-feature-glow"></canvas>
    <div class="chart-feature-media skel" id="chart-feature-media"></div>
    <div class="chart-feature-info">
      <div class="chart-feature-kicker">#1 in ${esc(chart)}</div>
      <div class="chart-feature-name">${esc(g.name || 'Unknown')}</div>
      <div class="chart-feature-stats">
        ${typeof g.playerCount === 'number' ? `<span><span class="material-icons-round">people</span>${gvCompact(g.playerCount)} playing</span>` : ''}
        ${typeof g.likes === 'number' ? `<span><span class="material-icons-round">thumb_up</span>${g.likes}% liked</span>` : ''}
      </div>
      <button class="btn btn-primary">View game</button>
    </div>
  </div>`;
}
// A soft wash of a picture's colors. The blur is done once, on a canvas a few
// dozen pixels wide that the browser then stretches over the area. A CSS blur of
// the same size is worked out again on every frame the page scrolls.
// `pad` is how far past its box the wash spreads; the canvas is sized to match in CSS.
const GLOW_PX = 64;
function paintGlow(canvas, url, blur, pad = 0, done) {
  if (!canvas || !url) return;
  canvas._url = url;
  const box = canvas.parentElement;
  const W = box.offsetWidth || 900, H = box.offsetHeight || 360;
  const s = GLOW_PX / (W + 2 * pad);
  const img = new Image();
  img.onload = () => {
    if (canvas._url !== url || !img.naturalWidth) return; // a newer picture took over
    canvas.width = GLOW_PX;
    canvas.height = Math.max(1, Math.round((H + 2 * pad) * s));
    const ctx = canvas.getContext('2d');
    ctx.filter = `blur(${(blur * s).toFixed(2)}px) saturate(1.5)`;
    const bw = W * s, bh = H * s, fit = Math.max(bw / img.naturalWidth, bh / img.naturalHeight);
    const sw = bw / fit, sh = bh / fit; // the middle of the picture, cropped like background-size:cover
    ctx.drawImage(img, (img.naturalWidth - sw) / 2, (img.naturalHeight - sh) / 2, sw, sh, pad * s, pad * s, bw, bh);
    done?.();
  };
  img.src = url;
}

const _featureShots = {};
async function loadChartFeature(g) {
  const glow = document.getElementById('chart-feature-glow');
  if (glow && g.thumbUrl) paintGlow(glow, g.thumbUrl, 60);
  let url = _featureShots[g.universeId];
  if (url === undefined) {
    try {
      const r = await api.robloxGet(`https://thumbnails.roblox.com/v1/games/multiget/thumbnails?universeIds=${g.universeId}&countPerUniverse=1&defaults=true&size=768x432&format=Png&isCircular=false`);
      url = r?.data?.data?.[0]?.thumbnails?.find(t => t.state === 'Completed')?.imageUrl || '';
    } catch { url = ''; }
    _featureShots[g.universeId] = url;
  }
  const media = document.getElementById('chart-feature-media');
  if (!media) return;
  const src = url || g.thumbUrl;
  if (!src) { media.classList.remove('skel'); return; }
  if (glow && !glow._url) paintGlow(glow, src, 60); // no small thumbnail came with the game
  media.innerHTML = `<img src="${esc(src)}" alt=""/>`;
  const img = media.firstChild;
  img.onload = img.onerror = () => media.classList.remove('skel');
}

let _chartGameMap = {};
let _searchDebounce = null;
let _searchMode = false;

function renderCharts(games, searchMode) {
  const grid = document.getElementById('charts-grid');
  const emptyEl = document.getElementById('charts-empty');
  const loading = document.getElementById('charts-loading');
  loading.style.display = 'none';
  _chartGameMap = {};
  if (!games || !games.length) {
    emptyEl.style.display = 'flex';
    grid.style.display = 'none';
    return;
  }
  emptyEl.style.display = 'none';
  grid.style.display = 'grid';
  const featured = !searchMode && games.length > 4;
  grid.innerHTML = games.map((g, i) => {
    _chartGameMap[i] = g;
    if (featured && i === 0) return chartFeatureHtml(g);
    const players = typeof g.playerCount === 'number' ? gvCompact(g.playerCount) + ' playing' : '';
    // Rank only means something against the chart order; a search result list
    // has no ranking to show, so the overlay badge is skipped there instead of
    // labelling every card with the same "Search result" text.
    const rankLabel = searchMode ? '' : `<div class="chart-card-rank">#${i + 1}</div>`;
    // src is set by the IntersectionObserver, not here: a chart is ~50 cards
    // of 512x512 PNG, and loading them all up front is what made this tab
    // expensive. The URL rides on data-src so it can be dropped and restored.
    const thumb = g.thumbUrl
      ? `<img class="chart-card-thumb" data-src="${esc(g.thumbUrl)}" alt="" onload="this.classList.add('loaded')" onerror="_chartThumbError(this)"/>`
      : `<div class="chart-card-thumb-ph"><span class="material-icons-round">videogame_asset</span></div>`;
    return `<div class="chart-card" style="animation-delay:${i * 12}ms" onclick="openGameModal(${i})" title="View game info">
      ${thumb}
      ${rankLabel}
      <div class="chart-card-body">
        <div class="chart-card-name">${esc(g.name || 'Unknown')}</div>
        <div class="chart-card-stat">${players ? `<span><span class="material-icons-round">people</span>${players}</span>` : ''}${typeof g.likes === 'number' ? `<span><span class="material-icons-round">thumb_up</span>${g.likes}%</span>` : ''}</div>
      </div>
    </div>`;
  }).join('');
  _chartObserveImages();
  if (featured) loadChartFeature(games[0]);
}

async function searchRobloxGames(query) {
  const sessionId = randomGuid();
  const url = `https://apis.roblox.com/search-api/omni-search?searchQuery=${encodeURIComponent(query)}&sessionId=${sessionId}`;
  const r = await api.robloxGet(url);
  if (!r.ok) throw new Error(`HTTP ${r.status}`);
  const d = r.data;

  // omni-search returns one "Game" group per result (each holding a single
  // content entry), so every Game group has to be collected; taking only the
  // first group is what limited search to a single game. The entries already
  // carry name, rootPlaceId and playerCount, so the games lookup below is
  // only a fallback for anything missing them.
  const games = (d.searchResults || [])
    .filter(s => s.contentGroupType === 'Game')
    .flatMap(s => s.contents || [])
    .filter(c => !c.isSponsored && (c.universeId || c.contentId));
  const seen = new Set();
  const entries = games.filter(c => {
    const uid = c.universeId || c.contentId;
    if (seen.has(uid)) return false;
    seen.add(uid);
    return true;
  });
  const universeIds = entries.map(c => c.universeId || c.contentId);
  if (!universeIds.length) return [];

  const detailMap = {};
  entries.forEach(c => { detailMap[c.universeId || c.contentId] = { rootPlaceId: c.rootPlaceId, name: c.name, playing: c.playerCount, likes: likePct(c.totalUpVotes, c.totalDownVotes) }; });
  const missing = universeIds.filter(uid => !detailMap[uid].rootPlaceId || !detailMap[uid].name);
  if (missing.length) {
    const detailsRes = await api.robloxGet(`https://games.roblox.com/v1/games?universeIds=${missing.join(',')}`);
    const details = detailsRes.ok ? detailsRes.data : { data: [] };
    (details.data || []).forEach(g => { detailMap[g.id] = { rootPlaceId: g.rootPlaceId, name: g.name, playing: g.playing }; });
  }

  // Fetch thumbnails
  let thumbMap = {};
  try {
    const thumbRes = await api.robloxGet(
      `https://thumbnails.roblox.com/v1/games/icons?universeIds=${universeIds.join(',')}&returnPolicy=PlaceHolder&size=512x512&format=Png&isCircular=false`
    );
    if (thumbRes.ok) {
      const td = thumbRes.data;
      (td.data || []).forEach(t => { thumbMap[t.targetId] = t.imageUrl; });
    }
  } catch {}

  return universeIds.map(uid => {
    const det = detailMap[uid] || {};
    return {
      universeId: uid,
      likes: det.likes ?? null,
      placeId: det.rootPlaceId,
      name: det.name,
      playerCount: det.playing,
      thumbUrl: thumbMap[uid] || ''
    };
  }).filter(g => g.placeId);
}

function filterCharts(val) {
  clearTimeout(_searchDebounce);
  const query = val.trim();
  if (!query) {
    _searchMode = false;
    if (chartsLoaded) renderCharts(allCharts[chartTab] || [], false);
    else {
      document.getElementById('charts-grid').style.display = 'none';
      document.getElementById('charts-empty').style.display = 'none';
      document.getElementById('charts-loading').style.display = 'flex';
    }
    return;
  }
  _searchMode = true;
  _searchDebounce = setTimeout(async () => {
    const grid = document.getElementById('charts-grid');
    const loading = document.getElementById('charts-loading');
    const emptyEl = document.getElementById('charts-empty');
    grid.style.display = 'none';
    emptyEl.style.display = 'none';
    loading.style.display = 'flex';
    try {
      const results = await searchRobloxGames(query);
      // Only apply if search box still has same value
      if (document.getElementById('chart-search').value.trim() === query) {
        renderCharts(results, true);
      }
    } catch(e) {
      console.error('Search error:', e);
      loading.style.display = 'none';
      emptyEl.style.display = 'flex';
    }
  }, 420);
}

function openGameModal(idx) {
  const g = _chartGameMap[idx];
  if (g) openGameView(g);
}


// ── Mixer (graphics / fps / volume / kill) ─────────────────────────────────
const FF_GFX = 'DFIntDebugFRMQualityLevelOverride';
const FF_FPS = 'DFIntTaskSchedulerTargetFps';
let _volTimer = null, _mixRunning = 0;

async function mixInit() {
  // Pull current values from saved Fast Flags + settings.
  let flags = {};
  try { flags = (await api.readFFlags()) || {}; } catch {}

  // Graphics
  const gfxRaw = flags[FF_GFX];
  const gfxAuto = (gfxRaw === undefined || gfxRaw === null || gfxRaw === '');
  document.getElementById('mix-gfx-auto').checked = gfxAuto;
  const gfxVal = clampInt(gfxRaw, 1, 21, 10);
  document.getElementById('mix-gfx').value = gfxVal;
  document.getElementById('mix-gfx-val').textContent = gfxAuto ? '' : gfxVal;
  document.getElementById('mix-gfx').disabled = gfxAuto;

  // FPS - read from GlobalBasicSettings_13.xml via new ipc
  try {
    const fpsCap = await api.readFpsCap();
    // fps_write stores "unlimited" (0) as 9999, which is what reads back, so
    // checking only for 0 showed the toggle off again after every restart.
    const fpsUnl = (fpsCap === 0 || fpsCap >= 9999);
    document.getElementById('mix-fps-unl').checked = fpsUnl;
    document.getElementById('mix-fps').value = fpsUnl ? 60 : Math.max(10, fpsCap || 60);
    document.getElementById('mix-fps-val').textContent = fpsUnl ? '\u221e' : (fpsCap || 60);
    document.getElementById('mix-fps').disabled = fpsUnl;
  } catch {}

  const fpsLock = document.getElementById('mix-fps-lock');
  if (fpsLock) fpsLock.checked = settings.fpsCapLock !== false;

  // Volume
  const vol = (typeof settings.masterVolume === 'number') ? settings.masterVolume : 100;
  document.getElementById('mix-vol').value = vol;
  document.getElementById('mix-vol-val').textContent = vol + '%';

  updateSliderFill(document.getElementById('mix-gfx'));
  updateSliderFill(document.getElementById('mix-fps'));
  updateSliderFill(document.getElementById('mix-vol'));
  mixRefreshRunning();
}

// FPS
function mixFpsInput(v) {
  // The slider steps by 1 so exact refresh rates (144, 165) are reachable;
  // snapping to 5s here used to undo every arrow-key press.
  v = Math.min(9999, Math.max(10, parseInt(v, 10) || 60));
  const sl = document.getElementById('mix-fps');
  document.getElementById('mix-fps-val').textContent = v;
  updateSliderFill(sl);
}
async function mixFpsUnlToggle() {
  const unl = document.getElementById('mix-fps-unl').checked;
  document.getElementById('mix-fps').disabled = unl;
  if (unl) {
    document.getElementById('mix-fps-val').textContent = '∞';
    await mixWriteFps(0, 'FPS set to unlimited');
  } else {
    mixFpsCommit();
  }
}

async function mixFpsCommit() {
  if (document.getElementById('mix-fps-unl').checked) return;
  const v = parseInt(document.getElementById('mix-fps').value, 10);
  document.getElementById('mix-fps-val').textContent = v;
  await mixWriteFps(v, 'FPS cap: ' + v);
}

// The backend remembers the written cap as the one its FPS guard protects,
// so a successful write also arms the protection (unless it's switched off).
async function mixWriteFps(cap, okMsg) {
  let res;
  try { res = await api.writeFpsCap(cap); } catch (e) { res = { ok: false, error: e?.message || String(e) }; }
  if (!res || res.ok === false) { toast((res && res.error) || 'Failed to set FPS cap', 'err'); return false; }
  settings.fpsCap = res.value;
  logEntry('info', 'system', okMsg, { fpsCap: res.value });
  toast(okMsg + (settings.fpsCapLock !== false ? ' (next launch, protected)' : ' (next launch)'), 'ok');
  return true;
}

async function mixFpsLockToggle() {
  const on = document.getElementById('mix-fps-lock').checked;
  settings.fpsCapLock = on;
  let saved = false;
  try { saved = await api.saveSettings({ fpsCapLock: on }); } catch {}
  if (!saved) { toast('Could not save FPS protection', 'err'); return; }
  toast(on ? 'FPS cap protected from overwrites' : 'FPS cap protection off', on ? 'ok' : 'err');
}

function clampInt(v, min, max, dflt) {
  const n = parseInt(v, 10);
  if (isNaN(n)) return dflt;
  return Math.max(min, Math.min(max, n));
}

function paintAcctSub() {
  const sub = document.getElementById('acct-sub');
  if (!sub) return;
  const saved = accounts.length;
  if (!saved) sub.textContent = 'No accounts saved';
  else if (_mixRunning > 0) sub.textContent = `${_mixRunning} running \u00b7 ${saved} saved`;
  else sub.textContent = `${saved} account${saved !== 1 ? 's' : ''} saved`;
}

function setRunningBadges(n) {
  paintAcctSub();
  const txt = n + ' running';
  const tb = document.getElementById('tb-running');
  if (tb) {
    tb.textContent = txt;
    tb.classList.toggle('live', n > 0);
    tb.style.display = n > 0 ? 'inline-flex' : 'none';
  }
}

let _statusPoll = null;
let _lastCountPushAt = 0;
async function pollRunningCount() {
  // Skip if a push from the backend landed recently.
  if (Date.now() - _lastCountPushAt < 6500) return;
  let n = 0;
  try { n = await api.getRunningCount(); } catch { n = 0; }
  _mixRunning = n;
  setRunningBadges(n);
}

// Reconciles card state against the backend's watched/alive list, in case a
// push event (onRobloxClosed etc) was missed or lagged.
async function pollWatchedIds() {
  let ids;
  try { ids = await api.getWatchedIds(); } catch { return; }
  const watched = new Set(ids);
  // Reconcile the launched set for every account, not just the ones with a
  // card on screen: an account hidden by search/filter otherwise kept a stale
  // state, which then drove the group Kill buttons, tracking and the filter.
  let setChanged = false;
  for (const a of accounts) {
    const live = watched.has(a.id);
    if (live !== _launchedIds.has(a.id)) {
      if (live) { _launchedIds.add(a.id); relayoutSoon(a.id); } else _launchedIds.delete(a.id);
      setChanged = true;
    }
  }
  // Cards that drop out of the active filter get pulled from the DOM
  // directly instead of a full render(), which would replay entrance
  // animations and flicker on every poll tick.
  const dropFromFilter = (_acctFilter === 'running' || _acctFilter === 'idle')
    ? id => (_acctFilter === 'running' ? !watched.has(id) : watched.has(id))
    : null;
  document.querySelectorAll('.card[data-id]').forEach(card => {
    const id = card.dataset.id;
    const shouldBeLive = watched.has(id);
    const isLive = card.classList.contains('is-live');
    if (shouldBeLive !== isLive) {
      card.classList.toggle('is-live', shouldBeLive);
      const dot = card.querySelector('.card-dot');
      if (dot) {
        dot.classList.toggle('launched', shouldBeLive);
        dot.title = shouldBeLive ? 'Launched' : 'Not launched';
      }
      if (shouldBeLive) { _launchedIds.add(id); relayoutSoon(id); } else _launchedIds.delete(id);
      if (dropFromFilter && dropFromFilter(id)) card.remove();
    }
  });
  if (setChanged) refreshPkgAvatarStatus();
  if (_layoutOn && !_launchedIds.size) { _layoutOn = false; _layoutUsed = null; paintLayoutBtn(); }
  syncRunTimes();
}
function pollStatus() {
  pollRunningCount();
  pollWatchedIds().then(pollMemoryUsage);
  pollSystemStats();
}

function formatPct(n) {
  return n === null || n === undefined ? '–' : (n < 10 ? n.toFixed(1) : Math.round(n)) + '%';
}
function setMeter(cell, percent, text) {
  cell.querySelector('.meter i').style.width = Math.max(0, Math.min(100, percent || 0)) + '%';
  cell.querySelector('b').textContent = text;
}

// Whole-PC load for the Accounts toolbar, only measured while it's on screen.
let _sysPollBusy = false, _sysPolls = 0;
async function pollSystemStats() {
  if (_sysPollBusy || _appAway || document.hidden || !document.getElementById('page-accounts').classList.contains('active')) return;
  _sysPollBusy = true;
  try { paintSystemStats((await api.getSystemStats()) || {}); } catch {} finally { _sysPollBusy = false; }
}
function paintSystemStats(s) {
  _sysPolls++;
  [['sys-cpu', s.cpu], ['sys-mem', s.memory], ['sys-gpu', s.gpu]].forEach(([id, value]) => {
    const cell = document.getElementById(id);
    setMeter(cell, value, formatPct(value));
    cell.classList.toggle('high', (value || 0) >= 90);
  });
  // A PC without GPU counters: the first reading is always empty, so wait for a later one.
  document.getElementById('sys-gpu').style.display = s.gpu === null && _sysPolls > 2 ? 'none' : '';
}

// Working-set readout on running cards. Skipped while nothing is running, so
// an idle app makes no extra backend calls.
let _memPollBusy = false;
let _lastMemUsage = {}; // kept so a re-render can repaint the readout at once
function formatMb(mb) {
  return mb >= 1024 ? (mb / 1024).toFixed(1) + ' GB' : Math.round(mb) + ' MB';
}
async function pollMemoryUsage() {
  if (_memPollBusy) return;
  if (!_launchedIds.size) {
    _lastMemUsage = {};
    paintMemUsage();
    return;
  }
  _memPollBusy = true;
  let usage = {};
  try { usage = (await api.getMemoryUsage()) || {}; } catch {} finally { _memPollBusy = false; }
  _lastMemUsage = usage;
  paintMemUsage();
}
function paintMemUsage() {
  const usage = _lastMemUsage;
  document.querySelectorAll('.card-mem').forEach(el => {
    const id = el.id.slice(4);
    const u = _launchedIds.has(id) ? usage[id] : null;
    if (!u) { el.textContent = ''; el.title = ''; return; }
    if (!el.firstChild) el.innerHTML = '<span class="stat-meter">CPU<span class="meter"><i></i></span><b></b></span><span class="stat-meter">Memory<span class="meter"><i></i></span><b></b></span>';
    const [cpuCell, memCell] = el.children;
    setMeter(cpuCell, u.cpuPercent, formatPct(u.cpuPercent));
    // Without a limit the bar is measured against 4 GB, a heavy client's usual ceiling.
    const pct = u.workingSetMb / (u.limitMb || 4096) * 100;
    setMeter(memCell, pct, formatMb(u.workingSetMb));
    memCell.classList.toggle('high', !!u.limitMb && pct >= 85);
    el.title = 'RAM in use: ' + formatMb(u.workingSetMb) + ' (private ' + formatMb(u.privateMb) + ')'
      + (u.limitMb ? ' · limit ' + formatMb(u.limitMb) : ' · no limit');
  });
  paintRunTimes();
}
// Every 3 seconds while the window is in view, every 15 while it's minimized or in
// the tray. The backend still pushes launches and closes the moment they happen;
// this poll only repairs anything those pushes missed.
const STATUS_POLL_MS = 3000, STATUS_POLL_AWAY_MS = 15000;
let _appAway = false;
async function appOutOfSight() {
  if (document.hidden) return true;
  try { return !!(await api.isOutOfSight?.()); } catch { return false; }
}
function startStatusPoll() {
  if (_statusPoll) return;
  _statusPoll = -1;
  let ticking = false;
  const tick = async () => {
    if (ticking) return; // one loop only, even when a wake-up lands mid-tick
    ticking = true;
    clearTimeout(_statusPoll);
    try {
      _appAway = await appOutOfSight();
      pollStatus();
    } finally {
      ticking = false;
      _statusPoll = setTimeout(tick, _appAway ? STATUS_POLL_AWAY_MS : STATUS_POLL_MS);
    }
  };
  // Coming back into view catches up at once instead of waiting out the slow wait.
  const wake = () => { if (_appAway) tick(); };
  document.addEventListener('visibilitychange', () => { if (!document.hidden) wake(); });
  window.addEventListener('focus', wake);
  tick();
}

async function mixRefreshRunning() {
  try {
    _mixRunning = await api.getRunningCount();
  } catch { _mixRunning = 0; }
  setRunningBadges(_mixRunning);
  // No guessing which accounts are live from lastUsed any more: the count now
  // only includes instances the backend actually tracks, and pollWatchedIds()
  // syncs the real set every 3s, so a guess could only mislabel cards.
}

// Merge a single key into the on-disk Fast Flags without disturbing others.
async function mixWriteFlag(key, value) {
  let flags = {};
  try { flags = (await api.readFFlags()) || {}; } catch {}
  if (value === null) delete flags[key];
  else flags[key] = String(value);
  try { await api.writeFFlags(flags); } catch {}
}

// ── Rendering engine (Settings > Performance) ────────────────────────────
// D3D9 removed; Roblox dropped that render path, forcing it now crashes
// the client with a missing-DLL error instead of falling back.
const RENDER_ENGINE_FLAGS = {
  d3d11: 'FFlagDebugGraphicsPreferD3D11',
  opengl: 'FFlagDebugGraphicsPreferOpenGL',
  vulkan: 'FFlagDebugGraphicsPreferVulkan',
};
const RENDER_ENGINE_LABELS = { '': 'Automatic', d3d11: 'Direct3D 11', opengl: 'OpenGL', vulkan: 'Vulkan' };

async function renderEngineInit() {
  let flags = {};
  try { flags = (await api.readFFlags()) || {}; } catch {}
  // Purge a stale D3D9 flag from before it was removed; forcing it now
  // crashes the client on launch instead of degrading gracefully.
  if ('FFlagDebugGraphicsPreferD3D9' in flags) {
    delete flags['FFlagDebugGraphicsPreferD3D9'];
    try { await api.writeFFlags(flags); } catch {}
  }
  let current = '';
  for (const [engine, flagName] of Object.entries(RENDER_ENGINE_FLAGS)) {
    if (String(flags[flagName]).toLowerCase() === 'true') { current = engine; break; }
  }
  renderEngineUpdateUI(current);
}

function renderEngineUpdateUI(engine) {
  const label = document.getElementById('cdd-rendereng-label');
  if (label) label.textContent = RENDER_ENGINE_LABELS[engine] || 'Automatic';
  document.querySelectorAll('#cdd-rendereng-menu .cdd-option').forEach(o =>
    o.classList.toggle('selected', o.dataset.value === engine));
}

async function setRenderEngine(engine) {
  let flags = {};
  try { flags = (await api.readFFlags()) || {}; } catch {}
  for (const flagName of Object.values(RENDER_ENGINE_FLAGS)) delete flags[flagName];
  if (engine && RENDER_ENGINE_FLAGS[engine]) flags[RENDER_ENGINE_FLAGS[engine]] = 'True';
  try { await api.writeFFlags(flags); } catch {}
  renderEngineUpdateUI(engine);
  closeAllCdd();
  toast('Rendering engine: ' + (RENDER_ENGINE_LABELS[engine] || 'Automatic') + ' (next launch)', 'ok');
}

// Smoothly fill the slider track up to the current value.
function updateSliderFill(el) {
  if (!el) return;
  const min = parseFloat(el.min) || 0, max = parseFloat(el.max) || 100, v = parseFloat(el.value);
  const pct = max > min ? ((v - min) / (max - min)) * 100 : 0;
  el.style.background = 'linear-gradient(90deg, var(--ac) ' + pct + '%, var(--s4) ' + pct + '%)';
}

// Delegated listener so slider fill stays correct without every call site remembering to update it.
document.addEventListener('input', e => {
  if (e.target.matches && e.target.matches('.fps-slider')) updateSliderFill(e.target);
});

// Graphics
function mixGfxInput(v) {
  document.getElementById('mix-gfx-val').textContent = v;
  updateSliderFill(document.getElementById('mix-gfx'));
}
function mixGfxAutoToggle() {
  const auto = document.getElementById('mix-gfx-auto').checked;
  document.getElementById('mix-gfx').disabled = auto;
  if (auto) {
    document.getElementById('mix-gfx-val').textContent = '';
    mixWriteFlag(FF_GFX, null);
    toast('Graphics set to Auto', 'ok');
  } else {
    mixGfxCommit();
  }
}
function mixGfxCommit() {
  if (document.getElementById('mix-gfx-auto').checked) return;
  const v = document.getElementById('mix-gfx').value;
  document.getElementById('mix-gfx-val').textContent = v;
  mixWriteFlag(FF_GFX, v);
  toast('Graphics quality: ' + v + ' (next launch)', 'ok');
}

// Volume - applies live while dragging (debounced so we don't spawn the helper
// on every drag tick), and saves + confirms on release.
function mixVolInput(v) {
  document.getElementById('mix-vol-val').textContent = v + '%';
  updateSliderFill(document.getElementById('mix-vol'));
  clearTimeout(_volTimer);
  _volTimer = setTimeout(() => { api.setRobloxVolume(parseInt(v, 10)); }, 90);
}
function mixVolCommit() {
  const v = parseInt(document.getElementById('mix-vol').value, 10);
  document.getElementById('mix-vol-val').textContent = v + '%';
  updateSliderFill(document.getElementById('mix-vol'));
  settings.masterVolume = v;
  api.saveSettings({ masterVolume: v });
  clearTimeout(_volTimer);
  _volTimer = setTimeout(async () => {
    const res = await api.setRobloxVolume(v);
    if (res && res.ok) {
      toast('Volume ' + v + '%', 'ok');
    } else {
      toast('Couldn\u2019t set volume' + (res && res.error ? ': ' + res.error : ''), 'err');
    }
  }, 60);
}

// Kill all
async function mixKillAll() {
  const btns = Array.from(document.querySelectorAll('.kill-roblox-btn'));
  if (!btns.length || btns[0].disabled) return;
  btns.forEach(b => { b.disabled = true; });
  // A rejected invoke used to leave every Kill button disabled until restart.
  let res;
  try { res = await api.killAllRoblox(); } catch (e) { res = { ok: false, error: e?.message || String(e) }; }
  // Reset all dots / launched state.
  _launchedIds.clear();
  document.querySelectorAll('.card.is-live').forEach(c => c.classList.remove('is-live'));
  document.querySelectorAll('.card-dot.launched').forEach(d => { d.classList.remove('launched'); d.title = 'Not launched'; });
  refreshPkgAvatarStatus();
  await mixRefreshRunning();
  btns.forEach(b => { b.disabled = false; });
  if (res && res.ok) {
    const n = res.killed || 0;
    // Only instances this app launched are killed, so say how many rather
    // than claiming everything Roblox-shaped on the machine is gone.
    let msg = n ? `Closed ${n} instance${n === 1 ? '' : 's'}` : 'No instances launched by MultiRoblox';
    if (res.untracked) msg += ` (${res.untracked} couldn't be identified)`;
    toast(msg, 'ok');
  } else toast('Kill failed' + (res && res.error ? ': ' + res.error : ''), 'err');
}

// Re-pushes current graphics/fps/volume values and confirms; doesn't touch
// any running process (that's the separate Kill all Roblox button).
async function mixApply() {
  const btn = document.getElementById('mix-relaunch-btn');
  if (btn.disabled) return;
  btn.disabled = true;
  const orig = btn.innerHTML;

  mixGfxCommit();
  mixFpsCommit();
  // parseInt: roblox_set_volume takes a number, and the raw slider string made
  // the invoke reject (silently, in this catch), so Apply never set volume.
  try { await api.setRobloxVolume(parseInt(document.getElementById('mix-vol').value, 10)); } catch {}

  btn.disabled = false;
  btn.innerHTML = orig;
  toast('Settings applied', 'ok');
}

async function mixTrimMemory(btnId = 'set-trim-btn') {
  const btn = document.getElementById(btnId);
  if (!btn || btn.disabled) return;
  btn.disabled = true;
  const orig = btn.innerHTML;

  let res;
  try { res = await api.trimRobloxMemory(); } catch { res = null; }

  btn.disabled = false;
  btn.innerHTML = orig;
  if (res && res.ok) {
    toast(res.total > 0 ? `Trimmed ${res.trimmed}/${res.total} instance(s)` : 'No running instances', 'ok');
  } else {
    toast('Trim failed', 'err');
  }
}

function genToggleKey() {
  const inp = document.getElementById('gen-apikey');
  const icon = document.getElementById('gen-eye-icon');
  if (inp.type === 'password') { inp.type = 'text'; icon.textContent = 'visibility_off'; }
  else { inp.type = 'password'; icon.textContent = 'visibility'; }
}

// Literal values BloxGen's API expects for `type`, confirmed against their
// own /api/prices response (which echoes these back as its object keys);
// the docs only ever show these as human-readable labels in prose, and
// guessing at slugs like "30day" instead would have silently 400'd.
const GEN_TYPES = [
  { value: 'alt', label: 'Alt', desc: 'Fresh alt accounts' },
  { value: '+30 days old', label: '+30 days old', desc: 'Accounts aged 30+ days' },
  { value: '+1 year old', label: '+1 year old', desc: 'Accounts aged 1+ year' },
  { value: '5+ years old', label: '5+ years old', desc: 'Accounts aged 5+ years' },
  { value: 'dump', label: 'Dump', desc: 'Dump accounts with extra account metadata' },
];
let _genType = 'alt';

// Account-type selection only applies to BloxGen (Altgen has no equivalent
// tiers in what this app sends it), so the row hides itself for an Altgen
// key instead of showing a control that does nothing.
function genOnKeyInput() {
  const apiKey = (document.getElementById('gen-apikey').value || '').trim();
  const row = document.getElementById('gen-type-row');
  if (row) row.style.display = apiKey.startsWith('BLOX-') ? '' : 'none';
}

function genSetType(value, label, desc) {
  _genType = value;
  const lbl = document.getElementById('cdd-gentype-label');
  const dsc = document.getElementById('gen-type-desc');
  if (lbl) lbl.textContent = label;
  if (dsc) dsc.textContent = desc;
  document.querySelectorAll('#cdd-gentype-menu .cdd-option').forEach(o => {
    o.classList.toggle('selected', o.dataset.value === value);
  });
  try { localStorage.setItem('bloxgen_type', value); } catch {}
  closeAllCdd();
}

// Provider is auto-detected from the key's prefix so the single API-key field
// works for either service: BLOX- = BloxGen, MG_ = Altgen (altgen.me/docs).
async function genCombo() {
  const apiKey = (document.getElementById('gen-apikey').value || '').trim();

  let provider;
  if (apiKey.startsWith('BLOX-')) provider = 'bloxgen';
  else if (apiKey.startsWith('MG_')) provider = 'altgen';
  else {
    toast('Enter a valid API key (BloxGen: BLOX-... or Altgen: MG_...)', 'err');
    return;
  }

  const btn = document.getElementById('gen-btn');
  const out = document.getElementById('gen-output');
  const btnLabel = btn ? btn.querySelector('span:last-child') : null;
  if (btn) { btn.disabled = true; if (btnLabel) btnLabel.textContent = 'Generating'; }

  try {
    const d = provider === 'bloxgen' ? await genBloxgen(apiKey) : await genAltgen(apiKey);
    if (d.cookie) d.cookie = normalizeCookie(d.cookie);

    out.value = d.username + ':' + d.password;
    out.select();

    const counter = document.getElementById('gen-counter');
    if (counter) counter.textContent = d.type ? ('Generated: ' + d.type + (d.cost != null ? ' · cost ' + d.cost : '')) : '';

    // Store in history
    if (_genClearPromise) await _genClearPromise; // let a pending Clear's disk write land first
    _lastGenData = d;
    _genHistory.unshift({ username: d.username, password: d.password, cookie: d.cookie });
    if (_genHistory.length > 500) _genHistory.length = 500; // bound the persisted history
    api.writeGenHistory(_genHistory).catch(() => {});
    _ghPrepend();

    navigator.clipboard.writeText(d.username + ':' + d.password).catch(() => {});

    if (btn) { btn.disabled = false; if (btnLabel) btnLabel.textContent = 'Generate'; }

  } catch (e) {
    toast(e.message || 'Generation failed', 'err');
    if (btn) { btn.disabled = false; if (btnLabel) btnLabel.textContent = 'Generate'; }
  }
}

async function genBloxgen(apiKey) {
  const resp = await fetch('https://core.bloxgen.net/api/generate', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ apiKey, type: _genType })
  });
  const data = await resp.json();
  if (!data.success) throw new Error(data.message || data.error || 'Generation failed');
  return data.data;
}

async function genAltgen(apiKey) {
  const res = await api.altgenGenerate(apiKey, 1);
  const data = res.data;
  if (!data || !data.success) {
    throw new Error((data && data.error && data.error.message) || 'Generation failed');
  }
  const acct = data.data && data.data.accounts && data.data.accounts[0];
  if (!acct) throw new Error('No account returned');
  return { username: acct.username, password: acct.password, cookie: acct.cookie };
}

let _genHistory = [];
let _lastGenData = null;

const GH_BATCH = 40;
let _ghRendered = 0;

function genRenderHistory() {
  const list = document.getElementById('gen-history-list');
  const sc = document.getElementById('gen-history-sc');
  if (!list || !sc) return;
  if (_genHistory.length === 0) {
    // The rows have to go too: _ghPrepend() on the next Generate would keep
    // any left here, pointing at the wrong _genHistory index.
    sc.classList.add('is-empty');
    list.innerHTML = '';
    _ghRendered = 0;
    return;
  }
  sc.classList.remove('is-empty');
  _ghRendered = 0;
  list.innerHTML = '';
  _ghAppendBatch(list);
  list.onscroll = () => {
    if (list.scrollTop + list.clientHeight >= list.scrollHeight - 20) _ghAppendBatch(list);
  };
}

function _ghAppendBatch(list) {
  const end = Math.min(_ghRendered + GH_BATCH, _genHistory.length);
  if (_ghRendered >= end) return;
  const frag = document.createDocumentFragment();
  for (let i = _ghRendered; i < end; i++) {
    const h = _genHistory[i];
    const row = document.createElement('div');
    row.className = 'gen-hist-item';
    row.dataset.idx = i;
    row.innerHTML =
      '<span class="gh-user"><span style="color:var(--t3)">User:</span> ' + esc(h.username) + '  <span style="color:var(--t3)">Pass:</span> ' + esc(h.password) + '</span>' +
      '<div class="gh-actions">' +
        '<button class="btn btn-ghost" title="Copy username:password"><span class="material-icons-round" style="font-size:15px">content_copy</span></button>' +
        '<button class="btn btn-ghost" title="Sign in and add to your accounts"><span class="material-icons-round" style="font-size:15px">person_add</span></button>' +
      '</div>';
    const btns = row.querySelectorAll('button');
    btns[0].onclick = () => genHistCopy(i);
    btns[1].onclick = () => genHistAdd(i);
    frag.appendChild(row);
  }
  list.appendChild(frag);
  _ghRendered = end;
}

function _ghPrepend() {
  const list = document.getElementById('gen-history-list');
  const sc = document.getElementById('gen-history-sc');
  if (!list || !sc) return;
  sc.classList.remove('is-empty');
  // Re-index existing rows so their onclick indices stay correct
  list.querySelectorAll('.gen-hist-item').forEach(row => {
    const old = +row.dataset.idx;
    const ni = old + 1;
    row.dataset.idx = ni;
    const btns = row.querySelectorAll('button');
    btns[0].onclick = () => genHistCopy(ni);
    btns[1].onclick = () => genHistAdd(ni);
  });
  _ghRendered++;
  const h = _genHistory[0];
  const row = document.createElement('div');
  row.className = 'gen-hist-item';
  row.dataset.idx = 0;
  row.innerHTML =
    '<span class="gh-user"><span style="color:var(--t3)">User:</span> ' + esc(h.username) + '  <span style="color:var(--t3)">Pass:</span> ' + esc(h.password) + '</span>' +
    '<div class="gh-actions">' +
      '<button class="btn btn-ghost" title="Copy username:password"><span class="material-icons-round" style="font-size:15px">content_copy</span></button>' +
      '<button class="btn btn-ghost" title="Sign in and add to your accounts"><span class="material-icons-round" style="font-size:15px">person_add</span></button>' +
    '</div>';
  const btns = row.querySelectorAll('button');
  btns[0].onclick = () => genHistCopy(0);
  btns[1].onclick = () => genHistAdd(0);
  list.insertBefore(row, list.firstChild);
  list.scrollTop = 0;
}

function genHistCopy(i) {
  const h = _genHistory[i];
  if (!h) return;
  navigator.clipboard.writeText(h.username + ':' + h.password).then(() => toast('Copied ' + h.username, 'ok'));
}

// Adds a generated account, preferring a fresh sign-in with its password. That
// session belongs to this connection, whereas the provider's cookie was issued
// to theirs and may not outlive the move. The cookie is only the fallback, for
// when the sign-in can't finish. Throws with the reason if neither works.
const LOGIN_WINDOW_CLOSED = 'Login window closed';
async function _genImportAccount(g) {
  let loginError = null;
  if (g.username && g.password) {
    const r = await api.loginWithCredentials(g.username, g.password);
    if (r && r.success && r.cookie) {
      return api.addAccount({ username: r.username || g.username, userId: r.userId || null, cookie: r.cookie, gameTarget: '', nickname: '' });
    }
    loginError = (r && r.error) || 'Login failed';
    if (loginError === LOGIN_WINDOW_CLOSED) return null; // the user backed out
  }
  const cookie = normalizeCookie(g.cookie);
  if (cookie) {
    const res = await api.validateCookie(cookie);
    if (res && res.ok) return api.addAccount({ username: res.username, userId: res.userId, cookie, gameTarget: '', nickname: '' });
    if (!loginError) loginError = (res && res.reason) || 'Cookie invalid or expired';
  }
  throw new Error(loginError || 'No password or cookie available for this account');
}

async function genHistAdd(i) {
  const h = _genHistory[i];
  if (!h) return;
  try {
    const a = await _genImportAccount(h);
    if (a) { accounts.push(a); render(); toast('Added ' + a.username + ' to accounts', 'ok'); }
  } catch(e) { toast('Failed to add: ' + (e.message || e), 'err'); }
}

async function genAddToAccounts() {
  if (!_lastGenData || !_lastGenData.username) { toast('No account to add', 'err'); return; }
  const btn = document.getElementById('gen-add-btn');
  const btnLabel = btn ? btn.querySelector('span:last-child') : null;
  if (btn) { btn.disabled = true; if (btnLabel) btnLabel.textContent = 'Adding'; }
  try {
    const a = await _genImportAccount(_lastGenData);
    if (a) { accounts.push(a); render(); toast('Added ' + a.username + ' to accounts!', 'ok'); }
  } catch(e) { toast('Failed: ' + (e.message || e), 'err'); }
  if (btn) { btn.disabled = false; if (btnLabel) btnLabel.textContent = 'Add'; }
}

// Tracked so a Generate right after Clear can wait for the clear's write to land first.
let _genClearPromise = null;
async function genClearHistory() {
  _genHistory = [];
  _lastGenData = null;
  genRenderHistory();
  toast('History cleared', 'ok');
  _genClearPromise = api.clearGenHistory().catch(() => {});
  await _genClearPromise;
  _genClearPromise = null;
}

function genDetailsCopy() {
  const details = document.getElementById('gen-details');
  const text = details ? details.innerText.replace('copy', '').trim() : '';
  if (!text) { toast('No details to copy', 'err'); return; }
  const btn = document.getElementById('gen-details-copy-btn');
  navigator.clipboard.writeText(text).then(() => {
    if (btn) { const s = btn.querySelector('span:last-child'); if(s){s.textContent='done'; setTimeout(()=>{s.textContent='details';},1500);} }
    toast('Details copied', 'ok');
  });
}

function genCopy() {
  const val = document.getElementById('gen-output').value;
  if (!val) { toast('Nothing to copy', 'err'); return; }
  const btn = document.getElementById('gen-copy-btn');
  navigator.clipboard.writeText(val).then(() => {
    if (btn) {
      const icon = btn.querySelector('.material-icons-round');
      if (icon) { icon.textContent = 'check'; setTimeout(() => { icon.textContent = 'content_copy'; }, 1500); }
    }
    toast('Copied to clipboard', 'ok');
  });
}

// ── Tracking (timed screenshots -> Discord webhook) ─────────────────────────
function getTrackingWebhookUrl() { return settings.trackingWebhookUrl || ''; }
function getTrackingIntervalSec() {
  const s = Number(settings.trackingIntervalSec);
  return Number.isFinite(s) ? Math.min(3600, Math.max(30, Math.round(s))) : 300;
}
function getTrackingTimedIds() { return new Set(Array.isArray(settings.trackingTimedIds) ? settings.trackingTimedIds : []); }
function getTrackingRegions(id) {
  const r = (settings.trackingRegions || {})[id];
  return Array.isArray(r) ? r : [];
}
function formatTrackingInterval(sec) {
  if (sec < 60) return sec + 's';
  return Math.round(sec / 60) + ' min';
}
// Strips orphaned tracking settings when an account is removed.
function forgetTrackingAccount(id) {
  let changed = false;
  const regions = Object.assign({}, settings.trackingRegions || {});
  if (id in regions) { delete regions[id]; settings.trackingRegions = regions; changed = true; }
  const timedIds = getTrackingTimedIds();
  if (timedIds.has(id)) { timedIds.delete(id); settings.trackingTimedIds = [...timedIds]; changed = true; }
  if (changed) api.saveSettings({ trackingRegions: settings.trackingRegions, trackingTimedIds: settings.trackingTimedIds });
}

function renderTrackingPage() {
  const urlInput = document.getElementById('tracking-webhook-url');
  if (urlInput) urlInput.value = getTrackingWebhookUrl();
  const interval = document.getElementById('tracking-interval');
  const intervalSec = getTrackingIntervalSec();
  if (interval) interval.value = intervalSec;
  const intervalVal = document.getElementById('tracking-interval-val');
  if (intervalVal) intervalVal.textContent = formatTrackingInterval(intervalSec);

  const container = document.getElementById('tracking-accounts');
  if (!container) return;
  const timedIds = getTrackingTimedIds();
  if (!accounts.length) {
    container.innerHTML = `<div class="empty"><span class="material-icons-round">radar</span><div class="empty-title">No accounts yet</div><div class="empty-sub">Add an account to capture its window and post it to your webhook.</div><button class="btn btn-primary" style="margin-top:8px" onclick="goTo('accounts');openLogin()">Add account</button></div>`;
    return;
  }
  container.innerHTML = accounts.map(a => {
    const regions = getTrackingRegions(a.id);
    const metaText = regions.length ? (regions.length + ' spot' + (regions.length !== 1 ? 's' : '')) : 'Full window';
    return `
    <div class="tracking-account${_launchedIds.has(a.id) ? ' live' : ''}">
      <span class="pm-av" id="tracking-av-${a.id}">${esc((a.username || '?')[0].toUpperCase())}</span>
      <div class="tracking-account-info">
        <div class="tracking-account-name">${esc(a.nickname || a.username || 'Unknown')}</div>
        <div class="tracking-account-meta"><span class="trk-dot"></span>${_launchedIds.has(a.id) ? 'Running' : 'Not running'} · ${metaText}</div>
      </div>
      <div class="tracking-account-actions">
        <label class="strip-afk" title="Include in timed capture">Auto<span class="toggle"><input type="checkbox" data-track-timed="${a.id}" onchange="toggleTimedTrackingAccount(${jsArg(a.id)})" ${timedIds.has(a.id) ? 'checked' : ''}/><span class="toggle-trk"></span></span></label>
        <button class="btn btn-ghost" title="Outline capture spots" onclick="openRegionPicker(${jsArg(a.id)})"><span class="material-icons-round" style="font-size:15px">crop</span>Spots</button>
        <button class="btn btn-ghost" title="Capture now" data-track-capture="${a.id}" onclick="captureTrackingNow(${jsArg(a.id)})" ${_launchedIds.has(a.id) ? '' : 'disabled'}><span class="material-icons-round" style="font-size:15px">photo_camera</span>Capture</button>
      </div>
    </div>`;
  }).join('');
  loadAvatarsBatch(accounts);
}

function trackingIntervalInput(v) {
  document.getElementById('tracking-interval-val').textContent = formatTrackingInterval(Number(v));
  updateSliderFill(document.getElementById('tracking-interval'));
}
function saveTrackingInterval() {
  const sec = Math.min(3600, Math.max(30, parseInt(document.getElementById('tracking-interval').value, 10) || 300));
  settings.trackingIntervalSec = sec;
  api.saveSettings({ trackingIntervalSec: sec });
  startTrackingLoop();
  toast('Tracking interval: ' + formatTrackingInterval(sec), 'ok');
}

async function saveTrackingWebhookUrl() {
  const input = document.getElementById('tracking-webhook-url');
  if (!input) return;
  const url = input.value.trim();
  // Clearing the field just turns tracking off; only a non-empty value has to
  // be a real Discord webhook (the backend enforces this too).
  if (url) {
    let check;
    try { check = await api.trackingValidateWebhook(url); } catch { check = null; }
    if (check && !check.ok) { toast(check.error || 'That webhook URL is not valid', 'err'); return; }
  }
  settings.trackingWebhookUrl = url;
  api.saveSettings({ trackingWebhookUrl: url });
  startTrackingLoop();
  toast(url ? 'Webhook saved' : 'Webhook cleared', 'ok');
}

function toggleTimedTrackingAccount(id) {
  const enabled = getTrackingTimedIds();
  const toggle = document.querySelector(`[data-track-timed="${id}"]`);
  if (toggle && toggle.checked) enabled.add(id); else enabled.delete(id);
  settings.trackingTimedIds = [...enabled];
  api.saveSettings({ trackingTimedIds: settings.trackingTimedIds });
  startTrackingLoop();
}

let _trackingTimer = null;
let _trackingRunning = false;
function startTrackingLoop() {
  if (_trackingTimer) clearInterval(_trackingTimer);
  _trackingTimer = null;
  if (!getTrackingTimedIds().size || !getTrackingWebhookUrl()) return;
  _trackingTimer = setInterval(captureTimedTracking, getTrackingIntervalSec() * 1000);
}

async function captureTimedTracking() {
  if (_trackingRunning) return;
  const webhookUrl = getTrackingWebhookUrl();
  if (!webhookUrl) return;
  _trackingRunning = true;
  try {
    for (const id of getTrackingTimedIds()) {
      if (!_launchedIds.has(id)) continue;
      const a = accounts.find(x => x.id === id);
      if (!a) continue;
      try { await api.trackingCaptureAndSend(id, a.nickname || a.username || id, webhookUrl, getTrackingRegions(id)); } catch {}
    }
  } finally {
    _trackingRunning = false;
  }
}

async function captureTrackingNow(id) {
  const webhookUrl = getTrackingWebhookUrl();
  if (!webhookUrl) { toast('Set a webhook URL first', 'err'); return; }
  const a = accounts.find(x => x.id === id);
  if (!a) return;
  const button = document.querySelector(`[data-track-capture="${id}"]`);
  if (button) button.disabled = true;
  try {
    const res = await api.trackingCaptureAndSend(id, a.nickname || a.username || id, webhookUrl, getTrackingRegions(id));
    if (res && res.ok) toast('Sent to webhook', 'ok');
    else toast((res && res.error) || 'Capture failed', 'err');
  } catch (e) {
    toast((e && e.message) || 'Capture failed', 'err');
  } finally {
    if (button) button.disabled = false;
  }
}

// ── Region picker: drag rectangles to outline capture spots, saved as 0-1 fractions of the window.
let _regionPickerAccountId = null;
let _regionPickerDrag = null;
let _regionPickerRects = [];

async function openRegionPicker(id) {
  const a = accounts.find(x => x.id === id);
  if (!a) return;
  if (!_launchedIds.has(id)) { toast('Launch this account first to outline a spot', 'err'); return; }
  _regionPickerAccountId = id;
  _regionPickerRects = getTrackingRegions(id).slice();
  openModal('m-region-picker');
  const img = document.getElementById('region-picker-img');
  img.removeAttribute('src');
  renderRegionBoxes();
  let res;
  try { res = await api.trackingCapturePreview(id); } catch { res = null; }
  // The picker may have been closed, or reopened for another account, while
  // the capture ran; this preview (or its error) no longer belongs on screen.
  if (_regionPickerAccountId !== id || !document.getElementById('m-region-picker').classList.contains('open')) return;
  if (!res || !res.ok) {
    toast((res && res.error) || 'Could not capture a preview', 'err');
    closeRegionPicker();
    return;
  }
  img.onload = renderRegionBoxes;
  img.src = res.dataUrl;
}

function closeRegionPicker() {
  closeModal('m-region-picker');
  _regionPickerAccountId = null;
  _regionPickerDrag = null;
}

function resetRegionPicker() {
  _regionPickerRects = [];
  renderRegionBoxes();
}

function renderRegionBoxes() {
  const img = document.getElementById('region-picker-img');
  const wrap = document.getElementById('region-picker-wrap');
  wrap.querySelectorAll('.region-picker-box').forEach(el => el.remove());
  if (!img.clientWidth || !img.clientHeight) return;
  _regionPickerRects.forEach((rect, i) => {
    const box = document.createElement('div');
    box.className = 'region-picker-box';
    box.dataset.idx = i;
    box.title = 'Click to remove';
    box.style.left = (rect.x * img.clientWidth) + 'px';
    box.style.top = (rect.y * img.clientHeight) + 'px';
    box.style.width = (rect.w * img.clientWidth) + 'px';
    box.style.height = (rect.h * img.clientHeight) + 'px';
    box.style.pointerEvents = 'auto';
    box.onclick = e => {
      e.stopPropagation();
      _regionPickerRects.splice(i, 1);
      renderRegionBoxes();
    };
    wrap.appendChild(box);
  });
}

function saveRegionPicker() {
  if (!_regionPickerAccountId) return;
  const regions = Object.assign({}, settings.trackingRegions || {});
  if (_regionPickerRects.length) regions[_regionPickerAccountId] = _regionPickerRects;
  else delete regions[_regionPickerAccountId];
  settings.trackingRegions = regions;
  api.saveSettings({ trackingRegions: regions });
  closeRegionPicker();
  renderTrackingPage();
  toast('Capture spots saved', 'ok');
}

(function initRegionPickerDrag() {
  const wrap = document.getElementById('region-picker-wrap');
  if (!wrap) return;
  let draftBox = null;
  wrap.addEventListener('mousedown', e => {
    if (e.target.classList.contains('region-picker-box')) return; // handled by its own onclick
    const img = document.getElementById('region-picker-img');
    if (!img.clientWidth) return;
    const rect = img.getBoundingClientRect();
    const startX = Math.min(Math.max(e.clientX - rect.left, 0), rect.width);
    const startY = Math.min(Math.max(e.clientY - rect.top, 0), rect.height);
    _regionPickerDrag = { startX, startY, imgRect: rect };
    draftBox = document.createElement('div');
    draftBox.className = 'region-picker-box region-picker-box-draft';
    draftBox.style.left = startX + 'px';
    draftBox.style.top = startY + 'px';
    draftBox.style.width = '0px';
    draftBox.style.height = '0px';
    wrap.appendChild(draftBox);
    e.preventDefault();
  });
  document.addEventListener('mousemove', e => {
    if (!_regionPickerDrag || !draftBox) return;
    const { startX, startY, imgRect } = _regionPickerDrag;
    const x = Math.min(Math.max(e.clientX - imgRect.left, 0), imgRect.width);
    const y = Math.min(Math.max(e.clientY - imgRect.top, 0), imgRect.height);
    draftBox.style.left = Math.min(x, startX) + 'px';
    draftBox.style.top = Math.min(y, startY) + 'px';
    draftBox.style.width = Math.abs(x - startX) + 'px';
    draftBox.style.height = Math.abs(y - startY) + 'px';
  });
  document.addEventListener('mouseup', () => {
    if (!_regionPickerDrag) return;
    const img = document.getElementById('region-picker-img');
    const left = draftBox ? parseFloat(draftBox.style.left) : 0;
    const top = draftBox ? parseFloat(draftBox.style.top) : 0;
    const w = draftBox ? parseFloat(draftBox.style.width) : 0;
    const h = draftBox ? parseFloat(draftBox.style.height) : 0;
    if (draftBox) { draftBox.remove(); draftBox = null; }
    _regionPickerDrag = null;
    if (w < 8 || h < 8 || !img.clientWidth || !img.clientHeight) return;
    _regionPickerRects.push({
      x: left / img.clientWidth,
      y: top / img.clientHeight,
      w: w / img.clientWidth,
      h: h / img.clientHeight,
    });
    renderRegionBoxes();
  });
})();

// ── Sliding hover pill for tab-style selectors ──
// One gray pill per bar follows the pointer from option to option (the springy
// motion is in styles.css). Active options don't get it; they have the white pill.
const HV_HOSTS = '#topnav, .tab-bar, .gv-seg, .view-toggle';
const HV_ITEMS = '.nav-item, .tab-btn, .gv-seg button, .vt';
function hvMove(host, item) {
  let pill = host._hv;
  if (!pill || !pill.isConnected) {
    pill = document.createElement('span');
    pill.className = 'hv-pill';
    pill.setAttribute('aria-hidden', 'true');
    host.prepend(pill);
    host._hv = pill;
  }
  host._hvItem = item;
  if (item.classList.contains('active') || item.disabled) { pill.style.opacity = '0'; return; }
  const h = host.getBoundingClientRect(), r = item.getBoundingClientRect();
  const appear = pill.style.opacity !== '1';
  if (appear) pill.style.transition = 'none'; // first hover: show in place, then slide from there
  pill.style.left = (r.left - h.left - host.clientLeft + host.scrollLeft) + 'px';
  pill.style.top = (r.top - h.top - host.clientTop + host.scrollTop) + 'px';
  pill.style.width = r.width + 'px';
  pill.style.height = r.height + 'px';
  if (appear) { void pill.offsetWidth; pill.style.transition = ''; }
  pill.style.opacity = '1';
}
document.addEventListener('pointerover', e => {
  if (!(e.target instanceof Element)) return;
  const item = e.target.closest(HV_ITEMS), host = item?.closest(HV_HOSTS);
  if (item && host) hvMove(host, item);
}, true);
document.addEventListener('pointerout', e => {
  if (!(e.target instanceof Element)) return;
  const host = e.target.closest(HV_HOSTS);
  if (host?._hv && !(e.relatedTarget instanceof Element && host.contains(e.relatedTarget))) host._hv.style.opacity = '0';
}, true);
document.addEventListener('click', e => {
  const host = e.target instanceof Element && e.target.closest(HV_HOSTS);
  if (host?._hvItem) setTimeout(() => hvMove(host, host._hvItem), 0); // the clicked option just became active
}, true);

// ── Tooltips ──
// Replaces the system tooltip (which can't be animated): any element with a title
// shows this one instead, after a short wait, popping in near the pointer's target.
(function initTips() {
  const tip = document.createElement('div');
  tip.id = 'ui-tip';
  tip.setAttribute('role', 'tooltip');
  document.body.append(tip);
  let cur = null, timer = 0;
  function hide() { clearTimeout(timer); timer = 0; cur = null; tip.classList.remove('show'); }
  function show(el, text) {
    if (!el.isConnected) return;
    const snap = tip.classList.contains('show'); // sliding from another tip
    tip.textContent = text;
    const r = el.getBoundingClientRect(), w = tip.offsetWidth, h = tip.offsetHeight;
    const below = r.top < h + 16;
    const x = Math.max(8, Math.min(innerWidth - w - 8, r.left + r.width / 2 - w / 2));
    const y = below ? r.bottom + 8 : r.top - h - 8;
    if (!snap) tip.classList.add('snap'); // from hidden: appear at the spot, only slide when moving between tips
    tip.style.setProperty('--tx', x + 'px');
    tip.style.setProperty('--ty', y + 'px');
    tip.style.setProperty('--to', `${Math.round(r.left + r.width / 2 - x)}px ${below ? '0' : '100%'}`);
    if (!snap) { void tip.offsetWidth; tip.classList.remove('snap'); }
    tip.classList.add('show');
  }
  document.addEventListener('pointerover', e => {
    if (!(e.target instanceof Element)) return;
    const el = e.target.closest('[title],[data-tip]');
    if (el === cur) return;
    if (!el) { hide(); return; }
    if (el.hasAttribute('title')) { el.dataset.tip = el.getAttribute('title'); el.removeAttribute('title'); }
    const text = el.dataset.tip;
    if (!text) { hide(); return; }
    clearTimeout(timer);
    cur = el;
    timer = setTimeout(() => show(el, text), tip.classList.contains('show') ? 60 : 420);
  }, true);
  document.addEventListener('pointerout', e => {
    if (cur && !(e.relatedTarget instanceof Element && cur.contains(e.relatedTarget))) hide();
  }, true);
  ['pointerdown', 'wheel', 'keydown', 'blur'].forEach(t => addEventListener(t, hide, true));
})();
