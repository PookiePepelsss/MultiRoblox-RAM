use std::collections::HashMap;
use std::sync::Mutex;
use tauri::AppHandle;
use tokio::process::Child;

pub struct AppState {
    pub app_handle: AppHandle,
    pub http: reqwest::Client,
    pub http_no_redirect: reqwest::Client,

    // ---- encryption session (mirrors main.js's _sessionPass/_cachedKey) ----
    pub session_pass: Mutex<Option<String>>,
    pub cached_key: Mutex<Option<[u8; 32]>>,
    pub cached_legacy_key: Mutex<Option<[u8; 32]>>,

    // ---- process control (native helper / launch / watch loop) ----
    pub mutex_child: Mutex<Option<Child>>,
    pub antiafk_child: Mutex<Option<Child>>,
    pub native_helper_path: Mutex<Option<Option<std::path::PathBuf>>>, // Some(None) = resolved-to-unavailable
    pub account_pids: Mutex<HashMap<String, u32>>,
    pub watched_accounts: Mutex<HashMap<String, i64>>, // accountId -> readyAt epoch ms
    pub miss_counts: Mutex<HashMap<String, u32>>,
    pub watch_handle: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    // accountId -> recent auto-relaunch-on-crash timestamps (ms), oldest
    // first. Bounds how often watch_tick's crash handler will relaunch a
    // given account so a bad cookie/account can't spin-loop launches.
    pub auto_relaunch_history: Mutex<HashMap<String, Vec<i64>>>,
    // Resident "watch" mode helper process (see native.rs) -- reports PIDs on
    // its own interval so watch_tick doesn't spawn a fresh process every poll
    // tick. None = not running or hasn't reported yet.
    pub watch_pid_child: Mutex<Option<Child>>,
    pub watch_pid_cache: Mutex<Option<std::collections::HashSet<u32>>>,

    // ---- roblox network caches ----
    pub csrf_cache: Mutex<HashMap<String, (String, i64)>>,
    pub ticket_cache: Mutex<HashMap<String, (String, i64)>>,
    pub last_launch_ts: Mutex<i64>,
    pub launch_lock: tokio::sync::Mutex<()>, // serializes launches (replaces the old _launchQueue chain)

    // ---- login flow cancellation ----
    pub login_cancel: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

impl AppState {
    pub fn new(app_handle: AppHandle) -> Self {
        Self {
            app_handle,
            http: reqwest::Client::builder().build().expect("reqwest client"),
            http_no_redirect: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("reqwest client (no redirect)"),
            session_pass: Mutex::new(None),
            cached_key: Mutex::new(None),
            cached_legacy_key: Mutex::new(None),
            mutex_child: Mutex::new(None),
            antiafk_child: Mutex::new(None),
            native_helper_path: Mutex::new(None),
            account_pids: Mutex::new(HashMap::new()),
            watched_accounts: Mutex::new(HashMap::new()),
            miss_counts: Mutex::new(HashMap::new()),
            watch_handle: Mutex::new(None),
            auto_relaunch_history: Mutex::new(HashMap::new()),
            watch_pid_child: Mutex::new(None),
            watch_pid_cache: Mutex::new(None),
            csrf_cache: Mutex::new(HashMap::new()),
            ticket_cache: Mutex::new(HashMap::new()),
            last_launch_ts: Mutex::new(0),
            launch_lock: tokio::sync::Mutex::new(()),
            login_cancel: Mutex::new(None),
        }
    }
}
