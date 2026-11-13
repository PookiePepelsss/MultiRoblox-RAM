// Private servers (listing, buying, managing) and choosing a different server
// for each account in a batch launch.
//
// Every call that acts for an account takes its id and reads the cookie from
// storage, so spending Robux can only ever happen as an account the app holds.
use crate::roblox_api::{self, UA};
use crate::state::AppState;
use crate::storage;
use futures::future::join_all;
use once_cell::sync::Lazy;
use rand::seq::SliceRandom;
use regex::Regex;
use reqwest::Method;
use serde_json::{json, Map, Value};
use std::time::Duration;

const GAMES: &str = "https://games.roblox.com";
/// Roblox's own limit on a private server's name.
const MAX_NAME_CHARS: usize = 50;
const MAX_PAGES: usize = 3;

static PRICE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"data-private-server-price="(\d+)""#).unwrap());

fn digits(value: &str) -> Result<&str, String> {
    let v = value.trim();
    if !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()) {
        Ok(v)
    } else {
        Err("Not a valid Roblox ID".into())
    }
}

fn account(state: &AppState, id: &str) -> Result<storage::AccountInfo, String> {
    let info = storage::account_info(state, id).ok_or("This account no longer exists.")?;
    if info.cookie.is_empty() {
        return Err("This account has no usable cookie.".into());
    }
    Ok(info)
}

/// One authenticated request, retrying once with the fresh token Roblox
/// answers with when the cached CSRF token has gone stale. A rejected token
/// never reaches the action behind it, so the retry can't double-spend.
async fn send(
    state: &AppState,
    method: Method,
    url: &str,
    cookie: &str,
    body: Option<&Value>,
) -> Result<(u16, String), String> {
    let mut token = roblox_api::get_csrf_token(state, cookie)
        .await
        .ok_or("Could not sign in to Roblox with this account (cookie expired or network problem).")?;
    for attempt in 0..2 {
        let mut request = state
            .http
            .request(method.clone(), url)
            .header("Cookie", format!(".ROBLOSECURITY={}", cookie))
            .header("X-CSRF-TOKEN", token.as_str())
            .header("User-Agent", UA.as_str())
            .header("Accept", "application/json")
            .header("Referer", "https://www.roblox.com")
            .timeout(Duration::from_secs(15));
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .map_err(|e| format!("Could not reach Roblox: {e}"))?;
        let status = response.status().as_u16();
        let fresh = response
            .headers()
            .get("x-csrf-token")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let text = response.text().await.unwrap_or_default();
        if status == 403 && attempt == 0 {
            if let Some(fresh) = fresh {
                roblox_api::invalidate_csrf(state, cookie);
                token = fresh;
                continue;
            }
        }
        return Ok((status, text));
    }
    Err("Roblox kept rejecting the request.".into())
}

fn failure(status: u16, body: &str) -> String {
    let message = roblox_api::extract_roblox_error(body);
    if message.trim().is_empty() {
        format!("Roblox returned HTTP {status}")
    } else {
        message
    }
}

fn is_success(status: u16) -> bool {
    (200..300).contains(&status)
}

async fn get_authed(state: &AppState, url: &str, cookie: &str) -> Result<Value, String> {
    let (status, body) = send(state, Method::GET, url, cookie, None).await?;
    if !is_success(status) {
        return Err(failure(status, &body));
    }
    serde_json::from_str(&body).map_err(|_| "Roblox sent an unexpected reply.".to_string())
}

fn server_summary(item: &Value, user_id: Option<&str>) -> Value {
    let owner_id = item
        .pointer("/owner/id")
        .and_then(Value::as_i64)
        .map(|n| n.to_string());
    json!({
        "id": item.get("vipServerId"),
        "name": item.get("name"),
        "accessCode": item.get("accessCode"),
        "playing": item.get("playing"),
        "maxPlayers": item.get("maxPlayers"),
        "ownerName": item.pointer("/owner/displayName").or_else(|| item.pointer("/owner/name")),
        "owned": owner_id.is_some() && owner_id.as_deref() == user_id,
    })
}

/// The account's private servers for a place, with the management details
/// Roblox only gives the owner.
pub async fn list(state: &AppState, account_id: &str, place_id: &str) -> Result<Value, String> {
    let place = digits(place_id)?;
    let info = account(state, account_id)?;
    let mut items: Vec<Value> = Vec::new();
    let mut restricted = false;
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut url = format!("{GAMES}/v1/games/{place}/private-servers");
        if let Some(c) = &cursor {
            url.push_str("?exclusiveStartKeyCursor=");
            url.push_str(&urlencoding::encode(c));
        }
        let page = get_authed(state, &url, &info.cookie).await?;
        restricted |= page
            .get("gameJoinRestricted")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if let Some(data) = page.get("data").and_then(Value::as_array) {
            items.extend(data.iter().cloned());
        }
        cursor = page
            .get("nextPageCursor")
            .and_then(Value::as_str)
            .filter(|c| !c.is_empty())
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }

    let user_id = info.user_id.as_deref();
    let details = items.iter().map(|item| {
        let mut summary = server_summary(item, user_id);
        let cookie = info.cookie.as_str();
        async move {
            let owned = summary.get("owned").and_then(Value::as_bool) == Some(true);
            let id = summary.get("id").and_then(Value::as_i64);
            if let (true, Some(id)) = (owned, id) {
                let url = format!("{GAMES}/v1/vip-servers/{id}");
                if let (Ok(detail), Some(obj)) =
                    (get_authed(state, &url, cookie).await, summary.as_object_mut())
                {
                    for key in ["joinCode", "link", "active", "subscription"] {
                        if let Some(v) = detail.get(key) {
                            obj.insert(key.into(), v.clone());
                        }
                    }
                }
            }
            summary
        }
    });
    let servers = join_all(details).await;
    Ok(json!({ "servers": servers, "gameJoinRestricted": restricted }))
}

fn parse_price(page: &str) -> Option<i64> {
    PRICE_RE.captures(page)?.get(1)?.as_str().parse().ok()
}

/// What a private server costs, read from the game page because the API has no
/// endpoint for it. Display only: the purchase states the price it expects, so
/// Roblox refuses it rather than charging a different amount.
pub async fn price(state: &AppState, place_id: &str) -> Result<Value, String> {
    let place = digits(place_id)?;
    let url = format!("https://www.roblox.com/games/{place}/game");
    let page = state
        .http
        .get(&url)
        .header("User-Agent", UA.as_str())
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("Could not reach Roblox: {e}"))?
        .text()
        .await
        .unwrap_or_default();
    Ok(json!({ "price": parse_price(&page) }))
}

fn clean_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Give the server a name.".into());
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(format!("Keep the name under {MAX_NAME_CHARS} characters."));
    }
    Ok(name.to_string())
}

/// Buys a private server. Spends the account's Robux.
pub async fn buy(
    state: &AppState,
    account_id: &str,
    universe_id: i64,
    name: &str,
    expected_price: i64,
    idempotency_key: &str,
) -> Result<Value, String> {
    if universe_id <= 0 || expected_price < 0 || idempotency_key.is_empty() {
        return Err("That purchase isn't valid.".into());
    }
    let name = clean_name(name)?;
    let info = account(state, account_id)?;
    let url = format!("{GAMES}/v1/games/vip-servers/{universe_id}");
    let body = json!({
        "name": name,
        "expectedPrice": expected_price,
        "isPurchaseConfirmed": true,
        "idempotencyKey": idempotency_key,
    });
    let (status, text) = send(state, Method::POST, &url, &info.cookie, Some(&body)).await?;
    if !is_success(status) {
        return Err(failure(status, &text));
    }
    let reply: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    Ok(json!({ "ok": true, "id": reply.get("vipServerId") }))
}

/// Only the fields Roblox documents for this call are forwarded.
fn update_body(patch: &Map<String, Value>) -> Result<Value, String> {
    let mut body = Map::new();
    if let Some(name) = patch.get("name").and_then(Value::as_str) {
        body.insert("name".into(), Value::String(clean_name(name)?));
    }
    for key in ["newJoinCode", "active"] {
        if let Some(flag) = patch.get(key).and_then(Value::as_bool) {
            body.insert(key.into(), Value::Bool(flag));
        }
    }
    if body.is_empty() {
        return Err("Nothing to change.".into());
    }
    Ok(Value::Object(body))
}

/// Renames a server, makes a new join link, or turns it on or off.
pub async fn update(
    state: &AppState,
    account_id: &str,
    server_id: i64,
    patch: &Map<String, Value>,
) -> Result<Value, String> {
    let body = update_body(patch)?;
    let info = account(state, account_id)?;
    let url = format!("{GAMES}/v1/vip-servers/{server_id}");
    let (status, text) = send(state, Method::PATCH, &url, &info.cookie, Some(&body)).await?;
    if !is_success(status) {
        return Err(failure(status, &text));
    }
    Ok(serde_json::from_str(&text).unwrap_or_else(|_| json!({ "ok": true })))
}

/// Turns monthly renewal on or off.
pub async fn set_renewal(
    state: &AppState,
    account_id: &str,
    server_id: i64,
    active: bool,
    price: i64,
) -> Result<Value, String> {
    let info = account(state, account_id)?;
    let url = format!("{GAMES}/v1/vip-servers/{server_id}/subscription");
    let body = json!({ "active": active, "price": price });
    let (status, text) = send(state, Method::PATCH, &url, &info.cookie, Some(&body)).await?;
    if !is_success(status) {
        return Err(failure(status, &text));
    }
    Ok(serde_json::from_str(&text).unwrap_or_else(|_| json!({ "ok": true })))
}

// ---- different servers for a batch ----

#[derive(Debug, Clone, PartialEq)]
struct PublicServer {
    id: String,
    ping: Option<i64>,
}

/// Servers on one page of the public list that still have room.
fn open_servers(page: &Value) -> Vec<PublicServer> {
    page.get("data")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|s| {
                    let id = s.get("id")?.as_str()?.to_string();
                    let playing = s.get("playing").and_then(Value::as_i64).unwrap_or(0);
                    let max = s.get("maxPlayers").and_then(Value::as_i64)?;
                    (playing < max).then(|| PublicServer {
                        id,
                        ping: s.get("ping").and_then(Value::as_i64),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `count` different server ids. A server with no ping reading sorts last.
fn pick_servers(mut servers: Vec<PublicServer>, closest: bool, count: usize) -> Vec<String> {
    if closest {
        servers.sort_by_key(|s| s.ping.unwrap_or(i64::MAX));
    } else {
        servers.shuffle(&mut rand::thread_rng());
    }
    servers.into_iter().take(count).map(|s| s.id).collect()
}

async fn fetch_open_servers(state: &AppState, place: &str, wanted: usize) -> Vec<PublicServer> {
    let mut found = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut url = format!(
            "{GAMES}/v1/games/{place}/servers/Public?sortOrder=Desc&excludeFullGames=true&limit=100"
        );
        if let Some(c) = &cursor {
            url.push_str("&cursor=");
            url.push_str(&urlencoding::encode(c));
        }
        let Ok(reply) = roblox_api::get_json_public(state, &url).await else {
            break;
        };
        let Some(page) = reply.get("data") else { break };
        found.extend(open_servers(page));
        cursor = page
            .get("nextPageCursor")
            .and_then(Value::as_str)
            .filter(|c| !c.is_empty())
            .map(str::to_owned);
        if cursor.is_none() || found.len() >= wanted * 3 {
            break;
        }
    }
    found
}

/// A launch target per account, each in its own server. `mode` is "random",
/// "closest" or "private". Accounts that can't be placed get an `error`.
pub async fn plan(
    state: &AppState,
    target: &str,
    mode: &str,
    account_ids: &[String],
) -> Result<Value, String> {
    let place = roblox_api::extract_place_id(target)
        .ok_or("Different servers needs a game ID or game link on the group.")?;
    let mut out = Map::new();

    if mode == "private" {
        let lookups = account_ids.iter().map(|id| {
            let place = place.as_str();
            async move {
                let reply = list(state, id, place).await;
                (id.clone(), reply)
            }
        });
        for (id, reply) in join_all(lookups).await {
            let assignment = match reply {
                Ok(listing) => match own_active_server(&listing) {
                    Some(code) => json!({ "target": format!("private:{place}:{code}") }),
                    None => json!({ "error": "This account has no active private server for this game." }),
                },
                Err(e) => json!({ "error": e }),
            };
            out.insert(id, assignment);
        }
        return Ok(Value::Object(out));
    }

    if mode != "random" && mode != "closest" {
        return Err("Unknown server mode.".into());
    }
    let servers = fetch_open_servers(state, &place, account_ids.len()).await;
    let mut jobs = pick_servers(servers, mode == "closest", account_ids.len()).into_iter();
    for id in account_ids {
        // More accounts than open servers: the rest let Roblox choose.
        let assignment = match jobs.next() {
            Some(job) => json!({ "target": format!("{place}:{job}") }),
            None => json!({ "target": place.clone(), "fallback": true }),
        };
        out.insert(id.clone(), assignment);
    }
    Ok(Value::Object(out))
}

fn own_active_server(listing: &Value) -> Option<String> {
    listing
        .get("servers")?
        .as_array()?
        .iter()
        .filter(|s| s.get("owned").and_then(Value::as_bool) == Some(true))
        .filter(|s| s.get("active").and_then(Value::as_bool) != Some(false))
        .find_map(|s| s.get("accessCode")?.as_str().map(str::to_owned))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(id: &str, ping: Option<i64>) -> PublicServer {
        PublicServer {
            id: id.into(),
            ping,
        }
    }

    #[test]
    fn ids_must_be_numeric() {
        assert_eq!(digits(" 1818 "), Ok("1818"));
        assert!(digits("18/../18").is_err());
        assert!(digits("").is_err());
    }

    #[test]
    fn price_comes_from_the_game_page() {
        assert_eq!(parse_price(r#"<div data-private-server-price="150" x>"#), Some(150));
        assert_eq!(parse_price(r#"<div data-private-server-price="0">"#), Some(0));
        assert_eq!(parse_price("<div>"), None);
    }

    #[test]
    fn full_servers_are_skipped() {
        let page = json!({ "data": [
            { "id": "a", "playing": 12, "maxPlayers": 12, "ping": 10 },
            { "id": "b", "playing": 3, "maxPlayers": 12, "ping": 90 },
            { "id": "c", "maxPlayers": 12 },
            { "playing": 1, "maxPlayers": 12 },
        ]});
        assert_eq!(open_servers(&page), vec![server("b", Some(90)), server("c", None)]);
    }

    #[test]
    fn closest_prefers_low_ping_and_every_pick_is_different() {
        let list = vec![server("slow", Some(200)), server("none", None), server("fast", Some(20))];
        assert_eq!(pick_servers(list.clone(), true, 2), ["fast", "slow"]);
        let mut random = pick_servers(list, false, 3);
        random.sort();
        random.dedup();
        assert_eq!(random.len(), 3);
    }

    #[test]
    fn only_owned_active_servers_are_used_for_private_launches() {
        let listing = json!({ "servers": [
            { "owned": false, "accessCode": "friend" },
            { "owned": true, "active": false, "accessCode": "off" },
            { "owned": true, "active": true, "accessCode": "mine" },
        ]});
        assert_eq!(own_active_server(&listing).as_deref(), Some("mine"));
        assert_eq!(own_active_server(&json!({ "servers": [] })), None);
    }

    #[test]
    fn updates_forward_only_known_fields() {
        let patch = json!({ "name": " Farm ", "active": false, "price": 1 });
        let body = update_body(patch.as_object().unwrap()).unwrap();
        assert_eq!(body, json!({ "name": "Farm", "active": false }));
        assert!(update_body(json!({ "price": 1 }).as_object().unwrap()).is_err());
        let long = "x".repeat(MAX_NAME_CHARS + 1);
        assert!(clean_name(&long).is_err());
        assert!(clean_name("  ").is_err());
    }
}
