// Port of main.js's ensureChrome() + puppeteerLogin(): drive a real Chromium
// browser through the Roblox login page over CDP and pull the .ROBLOSECURITY
// cookie once it appears. chromiumoxide replaces puppeteer-core; behaviour
// (browser detection order, stealth flags, cookie poll, timeouts) is
// unchanged from the Electron build.
use crate::state::AppState;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::network::{GetCookiesParams, SetCookieParams};
use futures::StreamExt;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

fn system_chrome_paths() -> Vec<PathBuf> {
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
    let pf86 = std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| r"C:\Program Files (x86)".into());
    let local = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| format!(r"{}\AppData\Local", home));
    vec![
        Path::new(&pf).join("Google").join("Chrome").join("Application").join("chrome.exe"),
        Path::new(&pf86).join("Google").join("Chrome").join("Application").join("chrome.exe"),
        Path::new(&local).join("Google").join("Chrome").join("Application").join("chrome.exe"),
        Path::new(&pf86).join("Microsoft").join("Edge").join("Application").join("msedge.exe"),
        Path::new(&pf).join("Microsoft").join("Edge").join("Application").join("msedge.exe"),
        Path::new(&pf).join("BraveSoftware").join("Brave-Browser").join("Application").join("brave.exe"),
        Path::new(&local).join("BraveSoftware").join("Brave-Browser").join("Application").join("brave.exe"),
    ]
}

fn chrome_cache_dir() -> PathBuf {
    crate::paths::app_data_dir().join("chrome-for-login")
}

// Finds a chrome.exe under the chrome-for-testing cache dir from a previous
// download (win64/chrome-win64/chrome.exe layout).
fn find_cached_chrome() -> Option<PathBuf> {
    let dir = chrome_cache_dir();
    if !dir.exists() {
        return None;
    }
    fn walk(dir: &Path, depth: u32) -> Option<PathBuf> {
        if depth > 4 {
            return None;
        }
        let entries = std::fs::read_dir(dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.file_name().and_then(|n| n.to_str()) == Some("chrome.exe") {
                return Some(path);
            }
        }
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(found) = walk(&path, depth + 1) {
                    return Some(found);
                }
            }
        }
        None
    }
    walk(&dir, 0)
}

async fn download_chrome(app: &AppHandle, state: &AppState) -> Option<PathBuf> {
    let _ = app.emit("chrome:download-progress", serde_json::json!({ "status": "downloading", "percent": 0 }));

    let versions_json: Value = state
        .http
        .get("https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions.json")
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let version = versions_json.get("channels")?.get("Stable")?.get("version")?.as_str()?.to_string();

    let platform = "win64"; // this app is Windows-only
    let url = format!(
        "https://storage.googleapis.com/chrome-for-testing-public/{}/{}/chrome-{}.zip",
        version, platform, platform
    );

    let cache_dir = chrome_cache_dir();
    let _ = std::fs::create_dir_all(&cache_dir);
    let zip_path = cache_dir.join("chrome.zip");

    let mut resp = state.http.get(&url).send().await.ok()?;
    let total = resp.content_length().unwrap_or(0);
    let mut downloaded: u64 = 0;
    let mut file = tokio::fs::File::create(&zip_path).await.ok()?;
    use tokio::io::AsyncWriteExt;
    while let Some(chunk) = resp.chunk().await.ok()? {
        file.write_all(&chunk).await.ok()?;
        downloaded += chunk.len() as u64;
        if total > 0 {
            let percent = ((downloaded as f64 / total as f64) * 100.0).round() as i64;
            let _ = app.emit("chrome:download-progress", serde_json::json!({ "status": "downloading", "percent": percent }));
        }
    }
    drop(file);

    let zip_path2 = zip_path.clone();
    let cache_dir2 = cache_dir.clone();
    let extracted = tokio::task::spawn_blocking(move || -> Option<PathBuf> {
        let file = std::fs::File::open(&zip_path2).ok()?;
        let mut archive = zip::ZipArchive::new(file).ok()?;
        archive.extract(&cache_dir2).ok()?;
        None::<PathBuf>
    })
    .await
    .ok()?;
    let _ = extracted;
    let _ = std::fs::remove_file(&zip_path);

    let _ = app.emit("chrome:download-progress", serde_json::json!({ "status": "done" }));
    find_cached_chrome()
}

async fn ensure_chrome(app: &AppHandle, state: &AppState) -> Option<PathBuf> {
    for p in system_chrome_paths() {
        if p.exists() {
            return Some(p);
        }
    }
    if let Some(p) = find_cached_chrome() {
        return Some(p);
    }
    download_chrome(app, state).await
}

pub struct LoginResult {
    pub success: bool,
    pub cookie: Option<String>,
    pub username: Option<String>,
    pub user_id: Option<String>,
    pub error: Option<String>,
}

pub async fn open_login(app: &AppHandle, state: &AppState) -> LoginResult {
    let Some(chrome_path) = ensure_chrome(app, state).await else {
        return LoginResult { success: false, cookie: None, username: None, user_id: None, error: Some("Failed to download Chrome. Check your internet connection and try again.".into()) };
    };
    browser_login(app, state, &chrome_path).await
}

// The login flow navigates the tab, spawns popups, and replaces the page
// during verification steps -- a session bound to one fixed target can go
// stale, so re-resolve a live page each tick (preferring whichever tab is
// actually on roblox.com) instead of polling a single page reference.
async fn try_get_cookie(browser: &Browser) -> Option<String> {
    let pages = browser.pages().await.ok()?;
    if pages.is_empty() {
        return None;
    }
    let mut target = None;
    for p in &pages {
        if let Ok(Some(url)) = p.url().await {
            if url.contains("roblox.com") {
                target = Some(p);
                break;
            }
        }
    }
    let target = target.unwrap_or_else(|| pages.last().unwrap());
    let resp = target.execute(GetCookiesParams::default()).await.ok()?;
    resp.result.cookies.iter().find(|c| c.name == ".ROBLOSECURITY" && c.domain.contains("roblox.com") && c.value.len() > 100).map(|c| c.value.clone())
}

async fn browser_login(_app: &AppHandle, state: &AppState, chrome_path: &Path) -> LoginResult {
    // chromiumoxide defaults to a FIXED shared profile dir (temp_dir()/
    // chromiumoxide-runner) when user_data_dir isn't set explicitly -- every
    // login reused whatever Roblox session cookie the previous login left
    // behind there, so "Add Account" kept auto-logging into the same account.
    // A fresh, unique dir per attempt is the fix; removed again once done.
    let profile_dir = std::env::temp_dir().join(format!("mr-login-{}", uuid::Uuid::new_v4()));

    let config = match BrowserConfig::builder()
        .chrome_executable(chrome_path)
        .with_head()
        .window_size(530, 700)
        .user_data_dir(&profile_dir)
        .args(vec!["--disable-blink-features=AutomationControlled"])
        .build()
    {
        Ok(c) => c,
        Err(e) => return LoginResult { success: false, cookie: None, username: None, user_id: None, error: Some(format!("Failed to launch Chrome: {}", e)) },
    };

    let (mut browser, mut handler) = match Browser::launch(config).await {
        Ok(b) => b,
        Err(e) => return LoginResult { success: false, cookie: None, username: None, user_id: None, error: Some(format!("Failed to launch Chrome: {}", e)) },
    };

    let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });

    let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();
    *state.login_cancel.lock().unwrap() = Some(cancel_tx);

    let result = async {
        if let Err(e) = browser.new_page("https://www.roblox.com/login").await {
            return LoginResult { success: false, cookie: None, username: None, user_id: None, error: Some(format!("Failed to launch Chrome: {}", e)) };
        };

        let started = std::time::Instant::now();
        let timeout = Duration::from_secs(5 * 60);
        loop {
            if started.elapsed() >= timeout {
                return LoginResult { success: false, cookie: None, username: None, user_id: None, error: Some("Timed out waiting for login. Please try again, or use \"Paste Cookie\".".into()) };
            }
            tokio::select! {
                _ = &mut cancel_rx => {
                    return LoginResult { success: false, cookie: None, username: None, user_id: None, error: Some("Login window closed".into()) };
                }
                found = try_get_cookie(&browser) => {
                    if let Some(cookie_val) = found {
                        let info = crate::roblox_api::fetch_user_info(state, &cookie_val).await;
                        if !info.ok {
                            return LoginResult { success: false, cookie: None, username: None, user_id: None, error: Some(info.reason.unwrap_or_else(|| "Could not verify account.".into())) };
                        }
                        return LoginResult { success: true, cookie: Some(cookie_val), username: info.username, user_id: info.user_id, error: None };
                    }
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                }
            }
        }
    }
    .await;

    *state.login_cancel.lock().unwrap() = None;
    let _ = browser.close().await;
    let _ = browser.wait().await;
    handler_task.abort();
    let _ = std::fs::remove_dir_all(&profile_dir); // best-effort, don't let cleanup failure mask the login result
    result
}

pub fn cancel_login(state: &AppState) {
    if let Some(tx) = state.login_cancel.lock().unwrap().take() {
        let _ = tx.send(());
    }
}

// "Open in browser" from a saved account's context menu: launches a real
// Chrome window (same detection/download as the login flow, but its own
// fresh temp profile so it doesn't touch the user's main browser profile),
// seeds the .ROBLOSECURITY cookie via CDP before any navigation happens so
// the page loads already authenticated, then leaves the window open for the
// user -- unlike browser_login() above, this never calls browser.close().
pub async fn open_account_in_browser(app: &AppHandle, state: &AppState, cookie: &str) -> Result<(), String> {
    let chrome_path = ensure_chrome(app, state).await.ok_or_else(|| "Failed to find or download Chrome".to_string())?;

    let config = BrowserConfig::builder()
        .chrome_executable(&chrome_path)
        .with_head()
        .args(vec!["--disable-blink-features=AutomationControlled"])
        .build()
        .map_err(|e| format!("Failed to launch Chrome: {}", e))?;

    let (browser, mut handler) = Browser::launch(config).await.map_err(|e| format!("Failed to launch Chrome: {}", e))?;
    tokio::spawn(async move { while handler.next().await.is_some() {} });

    let page = browser.new_page("about:blank").await.map_err(|e| e.to_string())?;
    let cookie_params = SetCookieParams::builder()
        .name(".ROBLOSECURITY")
        .value(cookie)
        .domain(".roblox.com")
        .url("https://www.roblox.com")
        .http_only(true)
        .secure(true)
        .build()
        .map_err(|e| e.to_string())?;
    page.execute(cookie_params).await.map_err(|e| e.to_string())?;
    page.goto("https://www.roblox.com/home").await.map_err(|e| e.to_string())?;

    std::mem::forget(browser); // detached: stays open for the user, matches child.forget() elsewhere
    Ok(())
}
