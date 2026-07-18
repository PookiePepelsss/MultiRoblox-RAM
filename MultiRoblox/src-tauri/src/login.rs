// "Add Account" login (open_login/browser_login below) uses a native Tauri
// window -- see the comment on open_login for why. "Open in browser" still
// drives a real, separately-downloaded Chromium over CDP (chromiumoxide,
// replacing puppeteer-core from the Electron build) since it's meant to be
// an actual full browser, not a login popup.
use crate::state::AppState;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::network::SetCookieParams;
use futures::StreamExt;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use url::Url;

fn system_chrome_paths() -> Vec<PathBuf> {
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
    let pf86 =
        std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| r"C:\Program Files (x86)".into());
    let local =
        std::env::var("LOCALAPPDATA").unwrap_or_else(|_| format!(r"{}\AppData\Local", home));
    vec![
        Path::new(&pf)
            .join("Google")
            .join("Chrome")
            .join("Application")
            .join("chrome.exe"),
        Path::new(&pf86)
            .join("Google")
            .join("Chrome")
            .join("Application")
            .join("chrome.exe"),
        Path::new(&local)
            .join("Google")
            .join("Chrome")
            .join("Application")
            .join("chrome.exe"),
        Path::new(&pf86)
            .join("Microsoft")
            .join("Edge")
            .join("Application")
            .join("msedge.exe"),
        Path::new(&pf)
            .join("Microsoft")
            .join("Edge")
            .join("Application")
            .join("msedge.exe"),
        Path::new(&pf)
            .join("BraveSoftware")
            .join("Brave-Browser")
            .join("Application")
            .join("brave.exe"),
        Path::new(&local)
            .join("BraveSoftware")
            .join("Brave-Browser")
            .join("Application")
            .join("brave.exe"),
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
    let _ = app.emit(
        "chrome:download-progress",
        serde_json::json!({ "status": "downloading", "percent": 0 }),
    );

    let versions_json: Value = state
        .http
        .get("https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions.json")
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let version = versions_json
        .get("channels")?
        .get("Stable")?
        .get("version")?
        .as_str()?
        .to_string();

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
            let _ = app.emit(
                "chrome:download-progress",
                serde_json::json!({ "status": "downloading", "percent": percent }),
            );
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

    let _ = app.emit(
        "chrome:download-progress",
        serde_json::json!({ "status": "done" }),
    );
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

// Switched from driving a separately-downloaded Chrome over CDP to a native
// Tauri window. Three problems with the old approach turned out to be
// unfixable from our side: Chrome 136+ forces the "being controlled by
// automated test software" banner and a full tabbed browser window whenever
// CDP is attached (Google hardened this specifically so automation can't
// hide a real browser UI from the user -- --app=<url> gets silently
// ignored), the standalone downloaded Chrome build rendered a solid black
// page for some users, and its pixel-based --window-size didn't account for
// the display's actual scale factor. A Tauri WebviewWindow (WebView2 on
// Windows) has none of this: no tabs/address bar ever (that's a browser-app
// concept, not something a plain window has), no CDP banner, respects DPI
// scaling automatically, and Tauri's own cookies_for_url() reads HttpOnly
// cookies directly -- no CDP needed at all. ensure_chrome/download_chrome
// below are kept only for "Open in browser", which genuinely wants a real,
// full browser.
pub async fn open_login(app: &AppHandle, state: &AppState) -> LoginResult {
    let label = format!("login-{}", uuid::Uuid::new_v4().simple());
    let login_url = match Url::parse("https://www.roblox.com/login") {
        Ok(u) => u,
        Err(e) => {
            return LoginResult {
                success: false,
                cookie: None,
                username: None,
                user_id: None,
                error: Some(e.to_string()),
            }
        }
    };

    let window = match tauri::WebviewWindowBuilder::new(app, &label, tauri::WebviewUrl::External(login_url))
        .title("Log in to Roblox")
        .inner_size(900.0, 720.0)
        .resizable(true)
        .center()
        .build()
    {
        Ok(w) => w,
        Err(e) => {
            return LoginResult {
                success: false,
                cookie: None,
                username: None,
                user_id: None,
                error: Some(format!("Failed to open login window: {}", e)),
            }
        }
    };

    let closed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let closed2 = closed.clone();
        window.on_window_event(move |event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                closed2.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        });
    }

    // state.login_cancel is a single global slot -- if a previous open_login
    // is still running (e.g. the UI's "Back" button just hides the browser
    // panel without cancelling it, so "Use browser" -> Back -> "Use browser"
    // again calls this while the first attempt is still in flight), just
    // overwriting it here would drop the old sender, which the old attempt
    // would read as its own cancellation on its next poll -- and then ITS
    // cleanup would null out the slot we just set for the new attempt,
    // permanently breaking Cancel for the new (still running) login. Cancel
    // the stale attempt explicitly first so ownership of the slot is never
    // ambiguous.
    if let Some(stale) = state.login_cancel.lock().unwrap().take() {
        let _ = stale.send(());
    }
    let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();
    *state.login_cancel.lock().unwrap() = Some(cancel_tx);

    let cookie_url = Url::parse("https://www.roblox.com").unwrap();
    let result = async {
        let started = std::time::Instant::now();
        let timeout = Duration::from_secs(5 * 60);
        loop {
            if started.elapsed() >= timeout {
                return LoginResult { success: false, cookie: None, username: None, user_id: None, error: Some("Timed out waiting for login. Please try again, or use \"Paste Cookie\".".into()) };
            }
            if closed.load(std::sync::atomic::Ordering::SeqCst) {
                return LoginResult { success: false, cookie: None, username: None, user_id: None, error: Some("Login window closed".into()) };
            }
            tokio::select! {
                _ = &mut cancel_rx => {
                    return LoginResult { success: false, cookie: None, username: None, user_id: None, error: Some("Login window closed".into()) };
                }
                _ = tokio::time::sleep(Duration::from_millis(1200)) => {
                    let found = window
                        .cookies_for_url(cookie_url.clone())
                        .ok()
                        .and_then(|cookies| {
                            cookies.into_iter().find(|c| {
                                c.name() == ".ROBLOSECURITY" && c.value().len() > 100
                            })
                        })
                        .map(|c| c.value().to_string());
                    if let Some(cookie_val) = found {
                        let info = crate::roblox_api::fetch_user_info(state, &cookie_val).await;
                        if !info.ok {
                            return LoginResult { success: false, cookie: None, username: None, user_id: None, error: Some(info.reason.unwrap_or_else(|| "Could not verify account.".into())) };
                        }
                        return LoginResult { success: true, cookie: Some(cookie_val), username: info.username, user_id: info.user_id, error: None };
                    }
                }
            }
        }
    }
    .await;

    // Deliberately NOT clearing state.login_cancel here -- if this attempt
    // was itself the one just cancelled by a newer open_login call (see the
    // "cancel any stale attempt" block above), the slot already belongs to
    // that newer attempt and clearing it unconditionally would rip its
    // cancel_tx out from under it. The slot only ever needs replacing (by
    // the next open_login call) or explicit cancelling (cancel_login), both
    // of which already .take() it safely; leaving a spent/stale sender
    // sitting there after a normal finish is harmless.
    let _ = window.close();
    result
}

pub fn cancel_login(state: &AppState) {
    if let Some(tx) = state.login_cancel.lock().unwrap().take() {
        let _ = tx.send(());
    }
}

// Leftover from when login used its own per-attempt temp Chrome profile dir
// (mr-login-*) -- login is a native Tauri window now and creates none, but
// this sweep is kept in case any still linger in %TEMP% from before an
// update, or a crash mid-login on an older build left one behind.
pub fn sweep_stale_login_profiles() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    let cutoff = std::time::SystemTime::now() - Duration::from_secs(6 * 60 * 60);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("mr-login-") {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if meta.modified().map(|t| t < cutoff).unwrap_or(false) {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
}

// "Open in browser" from a saved account's context menu: launches a real
// Chrome window (same detection/download as the login flow, but its own
// fresh temp profile so it doesn't touch the user's main browser profile),
// seeds the .ROBLOSECURITY cookie via CDP before any navigation happens so
// the page loads already authenticated, then leaves the window open for the
// user -- unlike browser_login() above, this never calls browser.close().
pub async fn open_account_in_browser(
    app: &AppHandle,
    state: &AppState,
    cookie: &str,
) -> Result<(), String> {
    let chrome_path = ensure_chrome(app, state)
        .await
        .ok_or_else(|| "Failed to find or download Chrome".to_string())?;

    let config = BrowserConfig::builder()
        .chrome_executable(&chrome_path)
        .with_head()
        .args(vec!["--disable-blink-features=AutomationControlled"])
        .build()
        .map_err(|e| format!("Failed to launch Chrome: {}", e))?;

    let (browser, mut handler) = Browser::launch(config)
        .await
        .map_err(|e| format!("Failed to launch Chrome: {}", e))?;
    tokio::spawn(async move { while handler.next().await.is_some() {} });

    let page = browser
        .new_page("about:blank")
        .await
        .map_err(|e| e.to_string())?;
    let cookie_params = SetCookieParams::builder()
        .name(".ROBLOSECURITY")
        .value(cookie)
        .domain(".roblox.com")
        .url("https://www.roblox.com")
        .http_only(true)
        .secure(true)
        .build()
        .map_err(|e| e.to_string())?;
    page.execute(cookie_params)
        .await
        .map_err(|e| e.to_string())?;
    page.goto("https://www.roblox.com/home")
        .await
        .map_err(|e| e.to_string())?;

    std::mem::forget(browser); // detached: stays open for the user, matches child.forget() elsewhere
    Ok(())
}
