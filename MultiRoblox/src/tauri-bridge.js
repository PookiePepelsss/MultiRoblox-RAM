// Reimplements the exact `window.api` surface the old Electron preload.js
// exposed, on top of Tauri's invoke/listen, so renderer.js needed zero
// call-site changes when the app moved off Electron.
(() => {
  const T = window.__TAURI__;
  const invoke = T.core.invoke;
  const listen = T.event.listen;
  const win = T.window.getCurrentWindow();

  // The top bar moves the window by hand instead of being a Windows drag area,
  // because a Windows drag area maximizes the window on a double-click.
  document.addEventListener('mousedown', e => {
    if (e.button !== 0 || e.detail > 1) return;
    const bar = e.target.closest?.('#titlebar');
    if (!bar || e.target.closest('button, a, input, select, textarea, [role="button"], #topnav, .tb-controls')) return;
    e.preventDefault();
    win.startDragging?.();
  });

  window.api = {
    minimize: () => win.minimize(),
    maximize: () => win.toggleMaximize(),
    // Raises CloseRequested, which the backend turns into a hide when
    // "hide to system tray" is on and a real close otherwise.
    close: () => win.close(),
    // Minimized or in the tray: nobody is looking, so background checks can slow down.
    isOutOfSight: async () => (await win.isMinimized()) || !(await win.isVisible()),
    quitApp: () => invoke('quit_app'),
    setHideToTray: (enabled) => invoke('set_hide_to_tray', { enabled }),

    loadAccounts: () => invoke('accounts_load'),
    addAccount: (account) => invoke('accounts_add', { account }),
    removeAccount: (id) => invoke('accounts_remove', { id }),
    updateAccount: (id, data) => invoke('accounts_update', { id, data }),
    reorderAccounts: (ids) => invoke('accounts_reorder', { ids }),

    loadPackages: () => invoke('packages_load'),
    savePackages: (packages) => invoke('packages_save', { packages }),

    openLogin: () => invoke('roblox_open_login'),
    loginWithCredentials: (username, password) => invoke('roblox_login_with_credentials', { username, password }),
    openAccountInBrowser: (cookie) => invoke('roblox_open_account_browser', { cookie }),
    cancelLogin: () => invoke('login_cancel'),
    validateCookie: (cookie) => invoke('roblox_validate_cookie', { cookie }),

    setRobloxVolume: (percent) => invoke('roblox_set_volume', { percent }),
    killAllRoblox: () => invoke('roblox_kill_all'),
    killOneRoblox: (id) => invoke('roblox_kill_one', { id }),
    getRunningCount: () => invoke('roblox_running_count'),
    getWatchedIds: () => invoke('roblox_watched_ids'),
    trimRobloxMemory: () => invoke('roblox_trim_memory'),
    trimAccountMemory: (id) => invoke('roblox_trim_account_memory', { id }),
    setAccountPriority: (id, priority) => invoke('roblox_set_account_priority', { id, priority }),
    // Per-account RAM cap in MB (0 = fall back to the global ramLimitMb
    // setting). Saved on the account and applied at once if it's running.
    setAccountRamLimit: (id, mb) => invoke('roblox_set_account_ram_limit', { id, mb: Number(mb) || 0 }),
    // { [accountId]: { pid, workingSetMb, privateMb, limitMb } } for running accounts.
    getMemoryUsage: () => invoke('roblox_memory_usage'),
    getSystemStats: () => invoke('system_stats'),
    layoutWindows: (on, options) => invoke('roblox_layout', { on, options }),
    windowPreviews: () => invoke('roblox_previews'),
    // Every detected Roblox install root, vanilla and bootstrappers alike.
    listInstalls: () => invoke('roblox_list_installs'),
    onAllRobloxClosed: (cb) => listen('roblox:allClosed', () => cb()),

    // persistTarget false launches there without changing the account's saved target.
    launchRoblox: (id, cookie, target, persistTarget = true) => invoke('roblox_launch', { id, cookie, target, persistTarget }),
    serversList: (id, placeId) => invoke('servers_list', { id, placeId }),
    serversPrice: (placeId) => invoke('servers_price', { placeId }),
    serversBuy: (id, universeId, name, expectedPrice, idempotencyKey) => invoke('servers_buy', { id, universeId, name, expectedPrice, idempotencyKey }),
    serversUpdate: (id, serverId, patch) => invoke('servers_update', { id, serverId, patch }),
    serversRenewal: (id, serverId, active, price) => invoke('servers_renewal', { id, serverId, active, price }),
    serversPlan: (target, mode, ids) => invoke('servers_plan', { target, mode, ids }),
    cancelLaunch: (id) => invoke('roblox_launch_cancel', { id }),
    openExternal: (url) => invoke('open_external', { url }),
    getAppVersion: () => invoke('app_version'),

    trackingCapturePreview: (id) => invoke('tracking_capture_preview', { id }),
    trackingCaptureAndSend: (id, username, webhookUrl, regions) => invoke('tracking_capture_and_send', { id, username, webhookUrl, regions }),
    trackingValidateWebhook: (url) => invoke('tracking_validate_webhook', { url }),

    loadSettings: () => invoke('settings_load'),
    saveSettings: (data) => invoke('settings_save', { data }),

    encStatus: () => invoke('enc_status'),
    encUnlock: (pass) => invoke('enc_unlock', { pass }),
    encSetKey: (pass) => invoke('enc_set_key', { pass }),
    clearAppData: () => invoke('clear_app_data'),

    multiInstanceStatus: () => invoke('multiinstance_status'),
    antiAfkStatus: () => invoke('antiafk_status'),

    readGenHistory: () => invoke('genhistory_read'),
    writeGenHistory: (list) => invoke('genhistory_write', { list }),
    clearGenHistory: () => invoke('genhistory_clear'),
    loadGenKey: () => invoke('gen_key_load'),
    saveGenKey: (key) => invoke('gen_key_save', { key }),

    readFFlags: () => invoke('fflag_read'),
    writeFFlags: (flags) => invoke('fflag_write', { flags }),
    readFpsCap: () => invoke('fps_read'),
    writeFpsCap: (cap) => invoke('fps_write', { cap }),

    onChromeProgress: (cb) => listen('chrome:download-progress', (e) => cb(e.payload)),
    onRobloxClosed: (cb) => listen('roblox:closed', (e) => cb(e.payload)),
    onRobloxStarted: (cb) => listen('roblox:started', (e) => cb(e.payload)),
    onRobloxCount: (cb) => listen('roblox:count', (e) => cb(e.payload)),
    onLogEntry: (cb) => listen('log:entry', (e) => cb(e.payload)),

    getGameName: (placeId, cookie) => invoke('roblox_get_game_name', { placeIdOrTarget: placeId, cookie }),
    // *.roblox.com sends no CORS headers; fetch() from the webview's real
    // https://tauri.localhost origin gets blocked (Electron's file:// origin was
    // exempt from this, which is why this needs a Rust-side detour).
    // Returns { ok, status, data } to mirror the fetch()+r.json() shape callers used.
    robloxGet: (url) => invoke('roblox_get_json', { url }),
    publicServersAs: (cookie, url) => invoke('roblox_public_servers_as', { cookie, url }),
    headshots: (tokens) => invoke('roblox_headshots', { tokens }),
    followUser: (cookie, username) => invoke('roblox_follow_user', { cookie, username }),
    // api.altgen.me sends no Access-Control-Allow-Origin either; same CORS
    // gap as robloxGet above. Returns { status, data } (data is the API's own
    // { success, message/error, data } JSON body).
    altgenGenerate: (apiKey, quantity) => invoke('altgen_generate', { apiKey, quantity }),
  };

  // Electron version showed the BrowserWindow only once the page had painted
  // (win.once('ready-to-show', ...)). Tauri's window starts hidden the same
  // way (see tauri.conf.json "visible": false); reveal it once the DOM is
  // actually ready instead of on window creation, so there's no white flash.
  const reveal = () => invoke('show_main_window').catch(() => {});
  if (document.readyState === 'complete' || document.readyState === 'interactive') reveal();
  else document.addEventListener('DOMContentLoaded', reveal);
})();
