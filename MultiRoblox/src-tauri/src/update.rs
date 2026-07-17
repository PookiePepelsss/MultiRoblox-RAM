// Checks the GitHub repo's tags for a newer version than this build. Uses
// the tags API (not /releases/latest) since tags are pushed as plain git
// tags, not formal GitHub Releases. Tags with letter suffixes (v1.0.8f,
// v1.0.8ff -- hotfix-on-hotfix versions pushed mid-session) are ignored;
// only strict vMAJOR.MINOR.PATCH tags count toward "latest".
use crate::state::AppState;
use serde_json::Value;

const REPO_API: &str = "https://api.github.com/repos/PookiePepelsss/MultiRoblox-RAM/tags";
const REPO_TAGS_URL: &str = "https://github.com/PookiePepelsss/MultiRoblox-RAM/tags";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

fn parse_semver(tag: &str) -> Option<(u32, u32, u32)> {
    // Must require the leading "v" -- a stray non-"v" tag on the remote
    // (e.g. a bare "1.1.2" left over from a since-corrected version bump)
    // was silently accepted here too, making it outrank the real "v1.1.1"
    // tag and falsely claim an update was available.
    let s = tag.strip_prefix('v')?;
    let mut parts = s.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None; // trailing junk (letter suffixes etc) -- not a clean release
    }
    Some((major, minor, patch))
}

pub async fn check_for_update(state: &AppState) -> Value {
    let current = parse_semver(CURRENT_VERSION).unwrap_or((0, 0, 0));

    let res = state
        .http
        .get(REPO_API)
        .header("User-Agent", "MultiRoblox-App")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await;

    let Ok(resp) = res else {
        return serde_json::json!({ "ok": false, "error": "network error" });
    };
    let Ok(tags): Result<Vec<Value>, _> = resp.json().await else {
        return serde_json::json!({ "ok": false, "error": "bad response" });
    };

    let latest = tags
        .iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
        .filter_map(parse_semver)
        .max();

    let Some(latest) = latest else {
        return serde_json::json!({ "ok": false, "error": "no version tags found" });
    };

    let update_available = latest > current;
    serde_json::json!({
        "ok": true,
        "current": CURRENT_VERSION,
        "latest": format!("{}.{}.{}", latest.0, latest.1, latest.2),
        "updateAvailable": update_available,
        "url": REPO_TAGS_URL,
    })
}
