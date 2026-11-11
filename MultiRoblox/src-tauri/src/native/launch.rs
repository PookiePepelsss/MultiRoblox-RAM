// The launch pipeline: auth, target resolution, spawn, bookkeeping.
use super::install::spawn_roblox_direct;
use super::process::{apply_priority_policy, cached_or_spawn_pids, close_singleton_handles};
use super::{daemon, emit_log, now_ms, watch};
use crate::state::AppState;
use crate::storage::{self, account_id as id_of, AccountInfo};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

const LAUNCH_STAGGER_MS: i64 = 800;

// ---- cancellation ----
// A launch can legitimately take ~30s: staggering, CSRF, ticket retries with
// backoff, then up to three spawn attempts; so it needs a way out. The flag
// is registered before the launch queues on launch_lock, so a launch that
// hasn't started yet can be abandoned too.
fn register_launch(state: &AppState, account_id: &str) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    state
        .launch_cancel
        .lock()
        .unwrap()
        .entry(account_id.to_string())
        .or_default()
        .push(flag.clone());
    flag
}

// Every in-flight launch of an account keeps its own flag. A second launch of
// the same account (a double click, or an auto-relaunch queued behind a
// manual one) used to replace the first one's flag, so Kill or removing the
// account cancelled only the newer launch and the older one still spawned.
fn finish_launch(state: &AppState, account_id: &str, flag: &Arc<AtomicBool>) {
    let mut map = state.launch_cancel.lock().unwrap();
    if let Some(flags) = map.get_mut(account_id) {
        flags.retain(|f| !Arc::ptr_eq(f, flag));
        if flags.is_empty() {
            map.remove(account_id);
        }
    }
}

/// Abandons every launch currently registered, in flight or still queued
/// behind launch_lock. Kill-all otherwise only reaps what has already
/// spawned: a package launch serializes on the lock with a stagger between
/// each, so killing partway through leaves the remaining ones to start up
/// afterwards, and instances reappear seconds after the user killed them.
pub(super) fn cancel_all_launches(state: &AppState) {
    for flag in state.launch_cancel.lock().unwrap().values().flatten() {
        flag.store(true, Ordering::SeqCst);
    }
}

pub fn cancel_launch(state: &AppState, account_id: &str) -> bool {
    match state.launch_cancel.lock().unwrap().get(account_id) {
        Some(flags) if !flags.is_empty() => {
            for flag in flags {
                flag.store(true, Ordering::SeqCst);
            }
            true
        }
        _ => false, // nothing in flight for this account
    }
}

fn launch_error(error: impl Into<String>) -> Value {
    json!({ "success": false, "error": error.into() })
}

fn cancelled_result() -> Value {
    json!({ "success": false, "cancelled": true, "error": "Launch cancelled" })
}

/// Registers the launch as cancellable, waits for its turn on launch_lock,
/// then runs it. Every launch goes through here, manual or automatic.
pub async fn launch_queued(
    app: &AppHandle,
    state: &AppState,
    account_id: &str,
    cookie: &str,
    target: &str,
    persist_target: bool,
) -> Value {
    let cancel = register_launch(state, account_id);
    let result = {
        let _guard = state.launch_lock.lock().await;
        if cancel.load(Ordering::SeqCst) {
            cancelled_result()
        } else {
            do_launch(app, state, account_id, cookie, target, persist_target, &cancel).await
        }
    };
    finish_launch(state, account_id, &cancel);
    result
}

// Waits between attempts to bring back an instance that closed. The usual
// reason a client closes is a network drop, and then the first attempt fails
// too: it needs the same connection for its CSRF token and auth ticket.
const RELAUNCH_RETRY_DELAYS: [Duration; 4] = [
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(120),
];

fn relaunch_finished(result: &Value) -> bool {
    let flag = |key: &str| result.get(key).and_then(Value::as_bool).unwrap_or(false);
    flag("success") || flag("cancelled") || flag("alreadyRunning")
}

/// True when this account's client is up, or was started so recently that it
/// may not show up as alive yet. A second client on the same account can get
/// the first one disconnected, and recording its PID would orphan the first
/// from Kill and the running count. A launch queued behind another one for the
/// same account (a double click, say) is stopped here too.
async fn already_running(app: &AppHandle, state: &AppState, account_id: &str) -> bool {
    let Some(pid) = state.account_pids.lock().unwrap().get(account_id).copied() else {
        return false;
    };
    let starting = state
        .watched_accounts
        .lock()
        .unwrap()
        .get(account_id)
        .is_some_and(|ready_at| now_ms() < *ready_at);
    if starting {
        return true;
    }
    // If the scan fails we can't tell, and a blocked launch is the cheap mistake.
    cached_or_spawn_pids(app, state)
        .await
        .is_none_or(|alive| alive.contains(&pid))
}

/// Sleeps for `delay` unless the flag is raised first. True if it was.
async fn wait_or_cancelled(delay: Duration, cancel: &AtomicBool) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed() < delay {
        if cancel.load(Ordering::SeqCst) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    cancel.load(Ordering::SeqCst)
}

/// Brings back an instance that closed by itself, retrying through an outage.
///
/// Its own cancel flag covers the waits between attempts, where no launch is
/// registered; without it, Kill All during a wait would be ignored and the
/// instance would reappear minutes later.
pub(super) async fn relaunch_with_retry(
    app: &AppHandle,
    state: &AppState,
    account_id: &str,
    name: &str,
    cookie: &str,
    target: &str,
) {
    let cancel = register_launch(state, account_id);
    let mut result = launch_queued(app, state, account_id, cookie, target, true).await;
    for delay in RELAUNCH_RETRY_DELAYS {
        if relaunch_finished(&result) {
            break;
        }
        emit_log(
            app,
            "info",
            "crash",
            &format!("Auto-relaunch of {name} failed, retrying in {}s", delay.as_secs()),
            None,
        );
        if wait_or_cancelled(delay, &cancel).await {
            finish_launch(state, account_id, &cancel);
            return;
        }
        // Each of these can change during a wait of up to two minutes.
        if !crate::settings::get_bool("autoRelaunch", false)
            || storage::account_info(state, account_id).is_none()
            || state.account_pids.lock().unwrap().contains_key(account_id)
        {
            finish_launch(state, account_id, &cancel);
            return;
        }
        result = launch_queued(app, state, account_id, cookie, target, true).await;
    }
    finish_launch(state, account_id, &cancel);
    if !relaunch_finished(&result) {
        let why = result.get("error").and_then(Value::as_str).unwrap_or("unknown error");
        emit_log(
            app,
            "err",
            "crash",
            &format!("Auto-relaunch of {name} gave up: {why}"),
            Some(json!({ "accountId": account_id })),
        );
    }
}

// ---- target resolution ----

// Compiled once rather than per launch; the patterns are constants, so the
// unwraps can only ever fire on first use, never mid-launch.
static JOB_SHORTHAND_RE: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
    regex::Regex::new(r"^(\d+)[,:]\s*([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})$").unwrap()
});
// "private:<placeId>:<accessCode>", for a server picked from the account's own
// list, where the access code is already known and no link needs resolving.
static PRIVATE_SHORTHAND_RE: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
    regex::Regex::new(r"^private:(\d+):([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})$").unwrap()
});
static PLACE_ID_GAMES_RE: once_cell::sync::Lazy<regex::Regex> =
    once_cell::sync::Lazy::new(|| regex::Regex::new(r"/games/(\d+)").unwrap());
static PLACE_ID_ANY_RE: once_cell::sync::Lazy<regex::Regex> =
    once_cell::sync::Lazy::new(|| regex::Regex::new(r"/(\d+)").unwrap());

fn job_launcher_url(place_id: &str, job_id: &str) -> String {
    format!(
        "https://assetgame.roblox.com/game/PlaceLauncher.ashx?request=RequestGameJob&placeId={}&gameId={}&isPlayTogetherGame=false",
        place_id, job_id
    )
}

fn place_launcher_url(place_id: &str) -> String {
    format!(
        "https://assetgame.roblox.com/game/placelauncher.ashx?request=RequestGame&placeId={}&isPlayTogetherGame=false",
        place_id
    )
}

/// What a game URL points at, before any network lookups.
#[derive(Debug, PartialEq)]
enum UrlTarget {
    /// A specific running server.
    Job {
        place_id: String,
        job_id: String,
    },
    /// A private server link; the access code has to be looked up.
    Private {
        place_id: String,
        link_code: String,
    },
    /// A share link (/share?code=...&type=Server); resolved via the API.
    Share {
        code: Option<String>,
    },
    Place(String),
}

/// Classifies a parsed game URL. Precedence matters and mirrors what the
/// launcher has always done: an explicit job id wins, then a private server
/// code, then share links, then a bare place.
fn classify_url(parsed: &url::Url) -> Result<UrlTarget, &'static str> {
    let query: HashMap<String, String> = parsed.query_pairs().into_owned().collect();
    let path = parsed.path();
    let place_id = PLACE_ID_GAMES_RE
        .captures(path)
        .or_else(|| PLACE_ID_ANY_RE.captures(path))
        .map(|c| c[1].to_string())
        .or_else(|| query.get("placeId").cloned());
    let job_id = ["jobId", "gameInstanceId", "serverJobId"]
        .iter()
        .find_map(|k| query.get(*k).cloned());
    let private_code = query.get("privateServerLinkCode").cloned();
    let share_code = query.get("code").cloned();

    match (place_id, job_id, private_code) {
        (Some(place_id), Some(job_id), _) => Ok(UrlTarget::Job { place_id, job_id }),
        (Some(place_id), None, Some(link_code)) => Ok(UrlTarget::Private {
            place_id,
            link_code,
        }),
        (place_id, _, _) => {
            if path == "/share" || (share_code.is_some() && query.contains_key("type")) {
                Ok(UrlTarget::Share { code: share_code })
            } else if let Some(place_id) = place_id {
                Ok(UrlTarget::Place(place_id))
            } else {
                Err("Could not find a Place ID in the URL.")
            }
        }
    }
}

/// Turns the user's target (blank, place id, "placeId:jobId", game URL,
/// private server or share link) into a PlaceLauncher URL. None = Roblox home.
async fn resolve_launcher_url(
    state: &AppState,
    target: &str,
    cookie: &str,
    csrf_token: &str,
) -> Result<Option<String>, String> {
    if target.is_empty() {
        return Ok(None);
    }
    // "placeId:jobId" or "placeId,jobId" shorthand for joining one specific
    // running server instance.
    if let Some(caps) = JOB_SHORTHAND_RE.captures(target) {
        return Ok(Some(job_launcher_url(&caps[1], &caps[2])));
    }
    if let Some(caps) = PRIVATE_SHORTHAND_RE.captures(target) {
        return Ok(Some(format!(
            "https://assetgame.roblox.com/game/PlaceLauncher.ashx?request=RequestPrivateGame&placeId={}&accessCode={}",
            &caps[1], &caps[2]
        )));
    }
    if target.chars().all(|c| c.is_ascii_digit()) {
        return Ok(Some(place_launcher_url(target)));
    }
    let mut raw_url = if target.starts_with("http") {
        target.to_string()
    } else {
        format!("https://{}", target)
    };
    if let Ok(parsed) = url::Url::parse(&raw_url) {
        let host = parsed.host_str().unwrap_or("");
        if host == "ro.blox.com" || host.ends_with(".ro.blox.com") {
            raw_url = crate::roblox_api::follow_redirect(state, &raw_url).await;
        }
    }
    let parsed = url::Url::parse(&raw_url)
        .map_err(|_| "Unrecognised input. Enter a place ID, game URL, or private server link.")?;
    match classify_url(&parsed)? {
        UrlTarget::Job { place_id, job_id } => Ok(Some(job_launcher_url(&place_id, &job_id))),
        UrlTarget::Place(place_id) => Ok(Some(place_launcher_url(&place_id))),
        UrlTarget::Private {
            place_id,
            link_code,
        } => {
            let access_code = crate::roblox_api::get_access_code(state, &place_id, &link_code, cookie)
                .await
                .ok_or("Could not resolve private server access code. The link may be expired or you may not have permission.")?;
            Ok(Some(format!(
                "https://assetgame.roblox.com/game/PlaceLauncher.ashx?request=RequestPrivateGame&placeId={}&accessCode={}&linkCode={}",
                place_id, access_code, link_code
            )))
        }
        UrlTarget::Share { code } => {
            let code = code.ok_or("Invalid share link: no code found.")?;
            let resolved =
                crate::roblox_api::resolve_share_link(state, &code, cookie, Some(csrf_token)).await;
            if !resolved.ok {
                return Err(resolved.error.unwrap_or_else(|| {
                    "Could not resolve share link. It may be expired or invalid.".into()
                }));
            }
            Ok(Some(format!(
                "https://assetgame.roblox.com/game/PlaceLauncher.ashx?request=RequestGameJob&placeId={}&isPlayTogetherGame=false&linkCode={}",
                resolved.place_id.unwrap_or_default(),
                resolved.link_code.unwrap_or_default()
            )))
        }
    }
}

fn roblox_uri(ticket: &str, launcher_url: Option<&str>) -> String {
    let launch_time = now_ms();
    let browser_id: u64 = rand::random::<u64>() % 9_000_000_000_000 + 1_000_000_000_000;
    // Always empty: an explicit channel makes the client go looking for that
    // channel's build instead of running the one on disk.
    let channel = "";
    match launcher_url {
        Some(url) => format!(
            "roblox-player:1+launchmode:play+gameinfo:{}+launchtime:{}+placelauncherurl:{}+browsertrackerid:{}+robloxLocale:en_us+gameLocale:en_us+channel:{}+LaunchExp:InApp",
            ticket, launch_time, urlencoding::encode(url), browser_id, channel
        ),
        None => format!(
            "roblox-player:1+launchmode:app+gameinfo:{}+launchtime:{}+browsertrackerid:{}+robloxLocale:en_us+gameLocale:en_us+channel:{}",
            ticket, launch_time, browser_id, channel
        ),
    }
}

// ---- launch ----

fn log_launch_failure(app: &AppHandle, account_id: &str, info: &AccountInfo, why: &str) {
    emit_log(
        app,
        "err",
        "launch",
        &format!("Launch failed for {}: {}", info.display(account_id), why),
        Some(json!({ "accountId": account_id, "username": info.username })),
    );
}

async fn do_launch(
    app: &AppHandle,
    state: &AppState,
    account_id: &str,
    cookie: &str,
    target: &str,
    persist_target: bool,
    cancel: &AtomicBool,
) -> Value {
    let cancelled = || cancel.load(Ordering::SeqCst);

    // Storage is the authority on which account this is, not the caller.
    // Every launch path hands us a cookie captured earlier from a copy of the
    // account: the launch modal's `launchAcc`, a package's resolved member
    // list, or the auto-relaunch task that snapshots cookie+target before it
    // queues on launch_lock. Any of those copies can outlive a delete, and
    // because the cookie alone decides which Roblox account the auth ticket
    // signs in, a stale one silently launches an account the user removed.
    // Re-read it here and refuse if it's gone.
    let Some(stored) = storage::account_info(state, account_id) else {
        emit_log(
            app,
            "warn",
            "launch",
            "Launch skipped: this account was removed",
            Some(json!({ "accountId": account_id })),
        );
        return launch_error("This account no longer exists.");
    };
    if already_running(app, state, account_id).await {
        return json!({
            "success": false,
            "alreadyRunning": true,
            "error": "This account is already running. Stop it first.",
        });
    }
    // Prefer the stored cookie over the caller's; they normally match, but if
    // the account was edited since the caller took its copy, stored is right.
    // Falls back to the passed one only when the stored cookie is unreadable
    // (failed decrypt leaves it empty), so this can't make a launch that used
    // to work start failing.
    let cookie: &str = if stored.cookie.is_empty() {
        cookie
    } else {
        &stored.cookie
    };

    // Mutex only here. The singleton-handle scan is deliberately NOT done
    // yet; see the call just before the spawn loop below.
    daemon::start_mutex_holder(app, state).await;
    daemon::ensure_singleton_held(app, state).await;
    if cancelled() {
        return cancelled_result();
    }

    let last = *state.last_launch_ts.lock().unwrap();
    let since_last = now_ms() - last;
    // A negative gap means the wall clock moved backwards; waiting it out
    // would hold launch_lock (and every queued launch) for that long.
    if last > 0 && (0..LAUNCH_STAGGER_MS).contains(&since_last) {
        tokio::time::sleep(Duration::from_millis(
            (LAUNCH_STAGGER_MS - since_last) as u64,
        ))
        .await;
    }
    if cancelled() {
        return cancelled_result();
    }

    let csrf_result = crate::roblox_api::get_csrf_token(state, cookie).await;

    let Some(csrf_token) = csrf_result else {
        // No token only means that request failed; a network drop or a rate
        // limit looks the same as a dead cookie here, so ask Roblox which.
        let info = crate::roblox_api::fetch_user_info(state, cookie).await;
        let why = if !info.ok && info.reachable {
            "Roblox rejected this account's cookie. It may have expired, or the account may be banned."
        } else {
            "Could not reach Roblox (network problem or rate limit). Try again in a moment."
        };
        log_launch_failure(app, account_id, &stored, why);
        return launch_error(why);
    };

    let ticket_result =
        crate::roblox_api::get_auth_ticket(state, cookie, Some(csrf_token.clone())).await;
    let ticket = match ticket_result.ticket {
        Some(ticket) if ticket_result.ok => ticket,
        _ => {
            let why = ticket_result.error.unwrap_or_default();
            log_launch_failure(
                app,
                account_id,
                &stored,
                &format!("auth ticket error - {}", why),
            );
            return launch_error(format!("Failed to get auth ticket: {}", why));
        }
    };
    // Last point before we start spawning processes; after this the client
    // is Roblox's to manage, and cancelling would mean killing it instead.
    if cancelled() {
        return cancelled_result();
    }

    let t = target.trim();
    let launcher_url = match resolve_launcher_url(state, t, cookie, &csrf_token).await {
        Ok(url) => url,
        Err(e) => {
            log_launch_failure(app, account_id, &stored, &e);
            return launch_error(e);
        }
    };
    let uri = roblox_uri(&ticket, launcher_url.as_deref());

    // closehandles (called on every spawn attempt below) closes singleton
    // handles in Roblox instances this app isn't tracking, so the new one can
    // start with a clean singleton state; tracked instances are exempt. The
    // brief moment while handles are closed can confuse a running instance
    // into crashing if the watch loop happens to tick right then and counts
    // the PID miss. Extend the grace window for every already-running account
    // so those misses are ignored for the duration of this launch sequence.
    {
        let grace_until = now_ms() + 8_000;
        let mut watched = state.watched_accounts.lock().unwrap();
        for (id, ready_at) in watched.iter_mut() {
            if id != account_id {
                *ready_at = (*ready_at).max(grace_until);
            }
        }
    }

    daemon::protect_sign_in_for_launch(app, state).await;

    let mut spawned_pid: Option<u32> = None;
    for attempt in 0..3 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(3)).await;
            // The retry backoff is the longest stretch of this whole path;
            // bail out here rather than making the user wait it out.
            if cancelled() {
                return cancelled_result();
            }
        }
        // Immediately before the spawn, on every attempt. A client launched
        // moments ago creates its own ROBLOX_singletonEvent a second or two
        // after it starts, so scanning any earlier (before the launch
        // stagger, the CSRF fetch and the auth-ticket retries) leaves a wide
        // window for a fresh handle to appear. Roblox then sees it on
        // startup and quits on its own, which surfaces here as an
        // unexplained "closed unexpectedly" for the second account.
        // The client reads its FPS cap once, at startup; a client that
        // exited moments ago may have just written its own value over it.
        if let Err(e) = crate::fps::enforce() {
            eprintln!("[fps] before launch: {e}");
        }
        close_singleton_handles(app, state).await;
        // Target resolution and the handle scan above can take seconds; a
        // Kill or removal that landed meanwhile must stop the spawn.
        if cancelled() {
            return cancelled_result();
        }
        spawned_pid = spawn_roblox_direct(&uri);
        if spawned_pid.is_some() {
            break;
        }
    }
    let Some(pid) = spawned_pid else {
        daemon::release_sign_in_if_idle(app, state).await;
        log_launch_failure(
            app,
            account_id,
            &stored,
            "no usable Roblox installation found",
        );
        return launch_error("No usable Roblox installation found. Install Roblox and try again.");
    };
    // A cancel that arrived during the spawn itself: the process exists but
    // the user already asked for it not to, so take it straight back down
    // rather than leave an instance nothing tracks.
    if cancelled() {
        super::process::taskkill(
            &[String::from("/PID"), pid.to_string()],
            Duration::from_secs(4),
        )
        .await;
        daemon::release_sign_in_if_idle(app, state).await;
        return cancelled_result();
    }
    state
        .account_pids
        .lock()
        .unwrap()
        .insert(account_id.to_string(), pid);
    // Fired the instant the process exists, not after the trailing
    // bookkeeping below or the JSON round-trip back to the caller; that gap,
    // stacked behind CSRF/ticket fetches and the inter-launch stagger for
    // every account after the first, is what made the UI's "launched" color
    // lag the real state of the world.
    let _ = app.emit("roblox:started", account_id);
    super::memory::schedule_after_launch(app, account_id, pid);
    // Stagger countdown starts from actual spawn time so trailing
    // bookkeeping (priority policy, storage write) counts toward it.
    *state.last_launch_ts.lock().unwrap() = now_ms();

    apply_priority_policy(state);
    crate::roblox_api::invalidate_ticket(state, cookie);

    let stamped = storage::update_accounts(state, |list| {
        let a = list.iter_mut().find(|a| id_of(a) == Some(account_id))?;
        let obj = a.as_object_mut()?;
        obj.insert(
            "lastUsed".into(),
            Value::String(chrono::Utc::now().to_rfc3339()),
        );
        if persist_target && !t.is_empty() {
            obj.insert("gameTarget".into(), Value::String(t.to_string()));
        }
        Some(AccountInfo::from_value(a))
    });
    let info = match stamped {
        Ok(info) => info.unwrap_or_default(),
        Err(e) => {
            eprintln!("[launch] could not record lastUsed: {e}");
            stored
        }
    };

    emit_log(
        app,
        "ok",
        "launch",
        &format!("Launched Roblox for {}", info.display(account_id)),
        Some(json!({
            "accountId": account_id,
            "username": info.username,
            "userId": info.user_id,
            "target": if t.is_empty() { "Roblox home" } else { t },
            "pid": pid,
        })),
    );

    watch::watch_roblox(app, account_id).await;

    if let Some(vol) = crate::settings::get_f64("masterVolume") {
        if vol != 100.0 {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_secs(9)).await;
                let st = app.state::<AppState>();
                super::process::set_roblox_volume(&app, &st, vol).await;
            });
        }
    }

    json!({ "success": true })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(s: &str) -> Result<UrlTarget, &'static str> {
        classify_url(&url::Url::parse(s).unwrap())
    }

    #[test]
    fn relaunch_retries_only_real_failures() {
        assert!(relaunch_finished(&json!({ "success": true })));
        assert!(relaunch_finished(&cancelled_result()));
        assert!(relaunch_finished(&json!({ "success": false, "alreadyRunning": true })));
        assert!(!relaunch_finished(&launch_error("no network")));
    }

    #[tokio::test]
    async fn waits_end_early_when_cancelled() {
        let raised = AtomicBool::new(true);
        let started = std::time::Instant::now();
        assert!(wait_or_cancelled(Duration::from_secs(60), &raised).await);
        assert!(started.elapsed() < Duration::from_secs(5));

        let clear = AtomicBool::new(false);
        assert!(!wait_or_cancelled(Duration::from_millis(1), &clear).await);
    }

    #[test]
    fn job_links_win() {
        assert_eq!(
            classify("https://www.roblox.com/games/start?placeId=1818&gameInstanceId=abc&privateServerLinkCode=x"),
            Ok(UrlTarget::Job { place_id: "1818".into(), job_id: "abc".into() })
        );
    }

    #[test]
    fn private_server_links() {
        assert_eq!(
            classify("https://www.roblox.com/games/1818/Name?privateServerLinkCode=777"),
            Ok(UrlTarget::Private {
                place_id: "1818".into(),
                link_code: "777".into()
            })
        );
    }

    #[test]
    fn share_links() {
        assert_eq!(
            classify("https://www.roblox.com/share?code=abc&type=Server"),
            Ok(UrlTarget::Share {
                code: Some("abc".into())
            })
        );
        assert_eq!(
            classify("https://www.roblox.com/share"),
            Ok(UrlTarget::Share { code: None })
        );
    }

    #[test]
    fn plain_game_links_and_misses() {
        assert_eq!(
            classify("https://www.roblox.com/games/1818/Classic-Crossroads"),
            Ok(UrlTarget::Place("1818".into()))
        );
        assert_eq!(
            classify("https://www.roblox.com/home"),
            Err("Could not find a Place ID in the URL.")
        );
    }

    #[test]
    fn private_shorthand_needs_a_place_and_access_code() {
        let code = "0123abcd-0123-4567-89ab-0123456789ab";
        assert!(PRIVATE_SHORTHAND_RE.is_match(&format!("private:1818:{code}")));
        assert!(!PRIVATE_SHORTHAND_RE.is_match("private:1818:nope"));
        assert!(!PRIVATE_SHORTHAND_RE.is_match(&format!("private:abc:{code}")));
    }

    #[test]
    fn job_shorthand_matches_both_separators() {
        let guid = "0123abcd-0123-4567-89ab-0123456789ab";
        assert!(JOB_SHORTHAND_RE.is_match(&format!("1818:{guid}")));
        assert!(JOB_SHORTHAND_RE.is_match(&format!("1818, {guid}")));
        assert!(!JOB_SHORTHAND_RE.is_match("1818"));
    }

    #[test]
    fn launch_uri_shapes() {
        let home = roblox_uri("T", None);
        assert!(home.starts_with("roblox-player:1+launchmode:app+gameinfo:T+"));
        let play = roblox_uri("T", Some("https://x/?a=1&b=2"));
        assert!(play.contains("+placelauncherurl:https%3A%2F%2Fx%2F%3Fa%3D1%26b%3D2+"));
        assert!(play.ends_with("+channel:+LaunchExp:InApp"));
    }
}
