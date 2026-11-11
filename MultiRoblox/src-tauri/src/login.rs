use crate::state::AppState;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::network::SetCookieParams;
use futures::StreamExt;
use serde::Serialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, WebviewWindow};
use tokio::sync::oneshot;
use url::Url;

fn system_chrome_paths() -> Vec<PathBuf> {
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
    let pf86 =
        std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| r"C:\Program Files (x86)".into());
    let local =
        std::env::var("LOCALAPPDATA").unwrap_or_else(|_| format!(r"{}\AppData\Local", home));
    let chrome = ["Google", "Chrome", "Application", "chrome.exe"];
    let edge = ["Microsoft", "Edge", "Application", "msedge.exe"];
    let brave = ["BraveSoftware", "Brave-Browser", "Application", "brave.exe"];
    [
        (&pf, chrome),
        (&pf86, chrome),
        (&local, chrome),
        (&pf86, edge),
        (&pf, edge),
        (&pf, brave),
        (&local, brave),
    ]
    .into_iter()
    .map(|(base, parts)| parts.iter().fold(PathBuf::from(base), |p, s| p.join(s)))
    .collect()
}

fn chrome_cache_dir() -> PathBuf {
    crate::paths::app_data_dir().join("chrome-for-login")
}

fn find_cached_chrome() -> Option<PathBuf> {
    fn walk(dir: &Path, depth: u32) -> Option<PathBuf> {
        if depth > 4 {
            return None;
        }
        let entries: Vec<PathBuf> = std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .collect();
        if let Some(exe) = entries
            .iter()
            .find(|p| p.is_file() && p.file_name().and_then(|n| n.to_str()) == Some("chrome.exe"))
        {
            return Some(exe.clone());
        }
        entries
            .iter()
            .filter(|p| p.is_dir())
            .find_map(|p| walk(p, depth + 1))
    }
    walk(&chrome_cache_dir(), 0)
}

fn chrome_progress(app: &AppHandle, payload: Value) {
    let _ = app.emit("chrome:download-progress", payload);
}

// Downloads into a staging folder and only moves the result into place once
// it has fully extracted. A failure used to leave a partial download (or a
// half-extracted tree that find_cached_chrome would happily pick up next
// time) and report nothing; now it says what went wrong and leaves nothing.
async fn download_chrome(app: &AppHandle, state: &AppState) -> Result<PathBuf, String> {
    chrome_progress(app, json!({ "status": "downloading", "percent": 0 }));
    let staging = crate::paths::app_data_dir().join("chrome-for-login.download");
    let result = download_into(app, state, &staging).await;
    let _ = tokio::fs::remove_dir_all(&staging).await;
    // "done" is the only terminal status the UI knows; sent on failure too so
    // the progress bar doesn't sit there forever. The error itself reaches
    // the UI through the command's result.
    chrome_progress(app, json!({ "status": "done" }));
    result.map_err(|e| format!("Chrome download failed: {e}"))
}

async fn download_into(
    app: &AppHandle,
    state: &AppState,
    staging: &Path,
) -> Result<PathBuf, String> {
    use tokio::io::AsyncWriteExt;

    let _ = tokio::fs::remove_dir_all(staging).await;
    tokio::fs::create_dir_all(staging)
        .await
        .map_err(|e| e.to_string())?;

    let versions_json: Value = state
        .http
        .get("https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions.json")
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let version = versions_json
        .pointer("/channels/Stable/version")
        .and_then(Value::as_str)
        .ok_or("version list has no Stable build")?;

    let platform = "win64"; // this app is Windows-only
    let url = format!(
        "https://storage.googleapis.com/chrome-for-testing-public/{}/{}/chrome-{}.zip",
        version, platform, platform
    );
    let mut resp = state
        .http
        .get(&url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| e.to_string())?;
    let total = resp.content_length().unwrap_or(0);

    let zip_path = staging.join("chrome.zip");
    let mut file = tokio::fs::File::create(&zip_path)
        .await
        .map_err(|e| e.to_string())?;
    let mut downloaded: u64 = 0;
    let mut last_percent = 0;
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;
        if total > 0 {
            let percent = ((downloaded as f64 / total as f64) * 100.0).round() as i64;
            // One event per percent, not per network chunk (thousands of them
            // for a ~150MB zip, each a round trip into the webview).
            if percent != last_percent {
                last_percent = percent;
                chrome_progress(app, json!({ "status": "downloading", "percent": percent }));
            }
        }
    }
    // tokio's File finishes writes in the background; without this the zip
    // can be opened for extraction before its tail has reached the disk.
    file.flush().await.map_err(|e| e.to_string())?;
    drop(file);

    let extract_dir = staging.join("extract");
    let zip_path2 = zip_path.clone();
    let extract_dir2 = extract_dir.clone();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let file = std::fs::File::open(&zip_path2).map_err(|e| e.to_string())?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        archive.extract(&extract_dir2).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;

    let dest = chrome_cache_dir();
    let _ = tokio::fs::remove_dir_all(&dest).await;
    tokio::fs::rename(&extract_dir, &dest)
        .await
        .map_err(|e| e.to_string())?;
    find_cached_chrome().ok_or_else(|| "chrome.exe missing from the download".to_string())
}

async fn ensure_chrome(app: &AppHandle, state: &AppState) -> Result<PathBuf, String> {
    if let Some(p) = system_chrome_paths().into_iter().find(|p| p.exists()) {
        return Ok(p);
    }
    if let Some(p) = find_cached_chrome() {
        return Ok(p);
    }
    download_chrome(app, state).await
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginResult {
    pub success: bool,
    pub cookie: Option<String>,
    pub username: Option<String>,
    pub user_id: Option<String>,
    pub error: Option<String>,
}

impl LoginResult {
    fn failed(error: impl Into<String>) -> Self {
        Self {
            success: false,
            cookie: None,
            username: None,
            user_id: None,
            error: Some(error.into()),
        }
    }
}

const LOGIN_URL: &str = "https://www.roblox.com/login";
const COOKIE_URL: &str = "https://www.roblox.com";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const COOKIE_POLL: Duration = Duration::from_millis(1200);
const COOKIE_SETTLE: Duration = Duration::from_millis(2500);
const WINDOW_CLOSED: &str = "Login window closed";

/// A JS string literal for `s`, safe to splice into injected script. JSON
/// escaping covers quotes, backslashes and control characters (a newline in
/// a password used to end the literal early); U+2028/U+2029 and '<' are
/// escaped as well, so the literal stays inert wherever the script ends up.
fn js_string(s: &str) -> String {
    serde_json::to_string(s)
        .unwrap_or_else(|_| "\"\"".into())
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
        .replace('<', "\\u003c")
}

// Wait for the login form to appear then fill and submit it. Roblox's login
// page is a React SPA; the inputs need React's own synthetic event system to
// register the value, not just a direct .value= assignment. The
// nativeInputValueSetter trick below fires the correct React onChange so the
// submit button becomes enabled.
fn credential_fill_script(username: &str, password: &str) -> String {
    format!(
        r#"
        (function tryFill() {{
            var u = document.getElementById('login-username');
            var p = document.getElementById('login-password');
            if (!u || !p) {{ setTimeout(tryFill, 300); return; }}
            var setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value').set;
            setter.call(u, {username});
            u.dispatchEvent(new Event('input', {{ bubbles: true }}));
            setter.call(p, {password});
            p.dispatchEvent(new Event('input', {{ bubbles: true }}));
            setTimeout(function() {{
                var btn = document.getElementById('login-button');
                if (btn) btn.click();
            }}, 300);
        }})();
    "#,
        username = js_string(username),
        password = js_string(password)
    )
}

struct LoginWindow {
    window: WebviewWindow,
    closed: Arc<AtomicBool>,
    cancel: oneshot::Receiver<()>,
}

// Native Tauri window instead of driving a downloaded Chrome over CDP;
// Chrome 136+ forces the "controlled by automated test software" banner and
// full tabbed UI whenever CDP is attached (--app=<url> gets ignored), so a
// clean chromeless login window isn't possible that way anymore. A
// WebviewWindow has no tabs/address bar by default and reads cookies via
// cookies_for_url() with no CDP needed.
fn open_login_window(
    app: &AppHandle,
    state: &AppState,
    title: &str,
) -> Result<LoginWindow, LoginResult> {
    let label = format!("login-{}", uuid::Uuid::new_v4().simple());
    let login_url = Url::parse(LOGIN_URL).map_err(|e| LoginResult::failed(e.to_string()))?;

    // incognito: a persistent cookie store here would leak the session
    // across login attempts, silently reusing whichever account was already
    // logged in instead of showing a fresh login page.
    let window =
        tauri::WebviewWindowBuilder::new(app, &label, tauri::WebviewUrl::External(login_url))
            .title(title)
            .inner_size(900.0, 720.0)
            .resizable(true)
            .center()
            .incognito(true)
            .build()
            .map_err(|e| LoginResult::failed(format!("Failed to open login window: {}", e)))?;

    let closed = Arc::new(AtomicBool::new(false));
    let closed2 = closed.clone();
    window.on_window_event(move |event| {
        if let tauri::WindowEvent::CloseRequested { .. } = event {
            closed2.store(true, Ordering::SeqCst);
        }
    });

    // Cancel any still-running previous attempt first; state.login_cancel
    // is a single slot, so a stray "Use browser" -> Back -> "Use browser"
    // sequence could otherwise clobber a still-live sender.
    let (cancel_tx, cancel) = oneshot::channel::<()>();
    if let Some(stale) = state.login_cancel.lock().unwrap().replace(cancel_tx) {
        let _ = stale.send(());
    }
    Ok(LoginWindow {
        window,
        closed,
        cancel,
    })
}

impl LoginWindow {
    fn session_cookie(&self, url: &Url) -> Option<String> {
        self.window
            .cookies_for_url(url.clone())
            .ok()?
            .into_iter()
            .find(|c| c.name() == ".ROBLOSECURITY" && c.value().len() > 100)
            .map(|c| c.value().to_string())
    }

    /// Polls the window's cookie jar until a .ROBLOSECURITY shows up, the
    /// user closes or cancels, or the timeout passes.
    async fn await_cookie(
        &mut self,
        state: &AppState,
        fill_script: Option<String>,
        timeout_msg: &str,
    ) -> LoginResult {
        let cookie_url = match Url::parse(COOKIE_URL) {
            Ok(u) => u,
            Err(e) => return LoginResult::failed(e.to_string()),
        };
        if let Some(script) = fill_script {
            // Give WebView2 time to paint before injecting.
            tokio::select! {
                _ = &mut self.cancel => return LoginResult::failed(WINDOW_CLOSED),
                _ = tokio::time::sleep(Duration::from_millis(1800)) => {}
            }
            let _ = self.window.eval(&script);
        }
        let started = std::time::Instant::now();
        loop {
            if started.elapsed() >= LOGIN_TIMEOUT {
                return LoginResult::failed(timeout_msg);
            }
            if self.closed.load(Ordering::SeqCst) {
                return LoginResult::failed(WINDOW_CLOSED);
            }
            tokio::select! {
                _ = &mut self.cancel => return LoginResult::failed(WINDOW_CLOSED),
                _ = tokio::time::sleep(COOKIE_POLL) => {}
            }
            if let Some(first) = self.session_cookie(&cookie_url) {
                // The page is still finishing the sign-in when the cookie first
                // appears, and closing the window straight away would keep a
                // value Roblox may be about to replace. Let it settle, then
                // take whatever the jar holds by then.
                tokio::select! {
                    _ = &mut self.cancel => return LoginResult::failed(WINDOW_CLOSED),
                    _ = tokio::time::sleep(COOKIE_SETTLE) => {}
                }
                let cookie = self.session_cookie(&cookie_url).unwrap_or(first);
                // A fresh login cookie is only checked for who it belongs to;
                // a rate limit or blip there (reachable:false) is retried
                // rather than throwing away a cookie the user just signed in for.
                let mut info = crate::roblox_api::fetch_user_info(state, &cookie).await;
                for _ in 0..3 {
                    if info.ok || info.reachable {
                        break;
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    info = crate::roblox_api::fetch_user_info(state, &cookie).await;
                }
                if !info.ok {
                    return LoginResult::failed(
                        info.reason
                            .unwrap_or_else(|| "Could not verify account.".into()),
                    );
                }
                return LoginResult {
                    success: true,
                    cookie: Some(cookie),
                    username: info.username,
                    user_id: info.user_id,
                    error: None,
                };
            }
        }
    }
}

async fn run_login(
    app: &AppHandle,
    state: &AppState,
    title: &str,
    fill_script: Option<String>,
    timeout_msg: &str,
) -> LoginResult {
    let mut lw = match open_login_window(app, state, title) {
        Ok(lw) => lw,
        Err(failed) => return failed,
    };
    let result = lw.await_cookie(state, fill_script, timeout_msg).await;
    // Not clearing state.login_cancel here; if a newer login call already
    // cancelled and replaced it, doing so would rip out its sender.
    let _ = lw.window.close();
    result
}

pub async fn open_login(app: &AppHandle, state: &AppState) -> LoginResult {
    run_login(
        app,
        state,
        "Log in to Roblox",
        None,
        "Timed out waiting for login. Please try again, or use \"Paste Cookie\".",
    )
    .await
}

pub async fn open_login_with_credentials(
    app: &AppHandle,
    state: &AppState,
    username: &str,
    password: &str,
) -> LoginResult {
    run_login(
        app,
        state,
        "Logging in...",
        Some(credential_fill_script(username, password)),
        "Timed out waiting for login.",
    )
    .await
}

pub fn cancel_login(state: &AppState) {
    if let Some(tx) = state.login_cancel.lock().unwrap().take() {
        let _ = tx.send(());
    }
}

const BROWSER_PROFILE_PREFIX: &str = "mr-login-";
// Where older builds kept one Chrome profile shared by every account, by not
// choosing a folder and getting chromiumoxide's fixed default.
const OLD_SHARED_BROWSER_PROFILE: &str = "chromiumoxide-runner";

// Cleans up temp Chrome profiles from earlier "Open in browser" sessions; each
// holds a signed-in Roblox session, so they shouldn't linger.
pub fn sweep_stale_login_profiles() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    let cutoff = std::time::SystemTime::now() - Duration::from_secs(6 * 60 * 60);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(BROWSER_PROFILE_PREFIX) && name != OLD_SHARED_BROWSER_PROFILE {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if meta.modified().map(|t| t < cutoff).unwrap_or(false) {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
}

// "Open in browser": launches a real Chrome window with the account's
// cookie pre-seeded, and leaves it open for the user.
pub async fn open_account_in_browser(
    app: &AppHandle,
    state: &AppState,
    cookie: &str,
) -> Result<(), String> {
    let chrome_path = ensure_chrome(app, state)
        .await
        .map_err(|e| format!("Failed to find or download Chrome. {e}"))?;

    // A profile of its own for each window. With a shared one, a second account
    // joined the Chrome already using it and overwrote that account's session.
    let profile = std::env::temp_dir().join(format!(
        "{BROWSER_PROFILE_PREFIX}{}",
        uuid::Uuid::new_v4().simple()
    ));

    let config = BrowserConfig::builder()
        .chrome_executable(&chrome_path)
        .user_data_dir(&profile)
        .with_head()
        // chromiumoxide emulates an 800x600 viewport by default, which draws
        // the page in a small box in the corner of a normal-sized window.
        .viewport(None)
        .args(vec!["--disable-blink-features=AutomationControlled"])
        .build()
        .map_err(|e| format!("Failed to launch Chrome: {}", e))?;

    let (browser, mut handler) = Browser::launch(config)
        .await
        .map_err(|e| format!("Failed to launch Chrome: {}", e))?;
    tauri::async_runtime::spawn(async move { while handler.next().await.is_some() {} });

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

    // Leaked on purpose: dropping the Browser closes the window it opened,
    // and this one belongs to the user now.
    std::mem::forget(browser);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_strings_escape_everything_that_could_break_out() {
        assert_eq!(
            js_string("a\"b\\c\nd\u{2028}e</script>"),
            r#""a\"b\\c\nd\u2028e\u003c/script>""#
        );
        assert_eq!(js_string(""), r#""""#);
    }

    #[test]
    fn fill_script_embeds_literals_not_raw_text() {
        let s = credential_fill_script("user\"); alert(1); (\"", "p\nw");
        assert!(s.contains(r#"setter.call(u, "user\"); alert(1); (\"");"#));
        assert!(s.contains(r#"setter.call(p, "p\nw");"#));
    }

    #[test]
    fn login_result_serialises_with_the_frontend_field_names() {
        let v = serde_json::to_value(LoginResult::failed("x")).unwrap();
        assert_eq!(
            v,
            json!({ "success": false, "cookie": null, "username": null, "userId": null, "error": "x" })
        );
    }
}
