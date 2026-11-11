// What the running clients' windows look like and where they sit: lining them up
// small in rows (Auto layout), and a tiny picture of each for its tile. Both are
// requests to the helper, which only makes ordinary window calls (SetWindowPos,
// PrintWindow); nothing is read from or injected into a client.
//
// The size trick that gets a window under Roblox's own minimum, and the idea of a
// small tiled grid, are from RoALT (MIT, Copyright (c) 2026 inspectsoftware); see
// the notes in RobloxNative.cs.
use crate::helper::{self, DEFAULT_TIMEOUT};
use crate::state::AppState;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tauri::AppHandle;

/// How Auto layout lays the windows out; what the dialog sends. Anything missing
/// or out of range falls back to the smallest, touching, top-left layout.
#[derive(Debug, Default, Deserialize, Clone, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct LayoutOptions {
    /// Client area of each window, in pixels.
    pub w: u32,
    pub h: u32,
    /// Where the first window goes: 0 top left, 1 top right, 2 bottom left, 3 bottom right.
    pub corner: u32,
    /// Pixels between windows; 0 = touching.
    pub gap: u32,
    /// Account ids to include; empty = every running one.
    pub ids: Vec<String>,
    /// Leave windows that are already lined up where they are, and only give the new
    /// ones the free cells (for a client that opens while the layout is on).
    pub append: bool,
}

impl LayoutOptions {
    /// The same options with every value held to what the helper accepts (pure).
    pub fn clamped(&self) -> LayoutOptions {
        let (w, h) = if self.w == 0 || self.h == 0 {
            (DEFAULT_W, DEFAULT_H)
        } else {
            (self.w.clamp(160, 1280), self.h.clamp(90, 720))
        };
        LayoutOptions { w, h, corner: self.corner.min(3), gap: self.gap.min(64), ids: self.ids.clone(), append: self.append }
    }
}

/// The smallest size offered: 16:9, so the previews match.
pub const DEFAULT_W: u32 = 320;
pub const DEFAULT_H: u32 = 180;
/// Previews are at most this wide, as JPEG at this quality: a couple of KB each.
const PREVIEW_W: u32 = 160;
const PREVIEW_QUALITY: u32 = 40;

/// (account id, pid) of every client this app launched and still sees, in a
/// stable order so the same accounts always land in the same cells.
fn running(state: &AppState) -> Vec<(String, u32)> {
    let mut v: Vec<(String, u32)> = state
        .account_pids
        .lock()
        .unwrap()
        .iter()
        .map(|(id, pid)| (id.clone(), *pid))
        .collect();
    v.sort();
    v
}

/// The helper request that tiles these processes (pure).
pub fn tile_command(pids: &[u32], o: &LayoutOptions) -> String {
    let o = o.clamped();
    let list: Vec<String> = pids.iter().map(u32::to_string).collect();
    format!("tile|{}|{}|{}|{}|{}|{}", list.join(","), o.w, o.h, o.corner, o.gap, u8::from(o.append))
}

/// The helper request for one window's picture (pure).
pub fn preview_command(pid: u32) -> String {
    format!("preview|{}|{}|{}", pid, PREVIEW_W, PREVIEW_QUALITY)
}

/// A picture the page can show as it is, or None when the helper had none to
/// give (minimized, or a window that can't draw itself this way) (pure).
pub fn preview_data_url(reply: &str) -> Option<String> {
    let r = reply.trim();
    if r.is_empty() || r == "MIN" || r == "BLANK" {
        return None;
    }
    Some(format!("data:image/jpeg;base64,{r}"))
}

/// The processes to lay out: those of the chosen accounts, or every running one (pure).
pub fn pids_for(running: &[(String, u32)], ids: &[String]) -> Vec<u32> {
    running
        .iter()
        .filter(|(id, _)| ids.is_empty() || ids.contains(id))
        .map(|(_, pid)| *pid)
        .collect()
}

/// Lines the running windows up as `options` say (`on`) or puts them back as they were.
pub async fn layout(app: &AppHandle, state: &AppState, on: bool, options: LayoutOptions) -> Value {
    if !cfg!(windows) {
        return json!({ "ok": false, "error": "Windows only" });
    }
    let cmd = if on {
        let pids = pids_for(&running(state), &options.ids);
        if pids.is_empty() {
            return json!({ "ok": false, "error": "No running accounts to lay out" });
        }
        tile_command(&pids, &options)
    } else {
        "untile".to_string()
    };
    match helper::call(app, state, &cmd, DEFAULT_TIMEOUT).await {
        Ok(n) => json!({ "ok": true, "count": n.trim().parse::<u32>().unwrap_or(0) }),
        Err(e) => json!({ "ok": false, "error": e }),
    }
}

/// A tiny picture of each running client's game, by account id. Clients with no
/// picture to give are left out, so the page keeps whatever it showed. Asks one
/// window at a time, and not at all while nothing is running (the helper isn't
/// started just for this).
pub async fn previews(app: &AppHandle, state: &AppState) -> Value {
    let mut out = Map::new();
    if !cfg!(windows) {
        return Value::Object(out);
    }
    for (id, pid) in running(state) {
        if let Ok(reply) = helper::call(app, state, &preview_command(pid), DEFAULT_TIMEOUT).await {
            if let Some(url) = preview_data_url(&reply) {
                out.insert(id, Value::String(url));
            }
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_every_pid_at_the_small_size_by_default() {
        let d = LayoutOptions::default();
        assert_eq!(tile_command(&[10, 20, 30], &d), "tile|10,20,30|320|180|0|0|0");
        assert_eq!(tile_command(&[], &d), "tile||320|180|0|0|0");
    }

    #[test]
    fn options_are_held_to_what_the_helper_takes() {
        let wild = LayoutOptions { w: 99999, h: 5, corner: 9, gap: 500, ..Default::default() };
        assert_eq!(tile_command(&[1], &wild), "tile|1|1280|90|3|64|0");
        let set = LayoutOptions { w: 480, h: 270, corner: 3, gap: 8, append: true, ..Default::default() };
        assert_eq!(tile_command(&[7, 8], &set), "tile|7,8|480|270|3|8|1");
        // only one of the two sizes given: the default size, not a sliver
        let half = LayoutOptions { w: 640, h: 0, ..Default::default() };
        assert_eq!(tile_command(&[1], &half), "tile|1|320|180|0|0|0");
    }

    #[test]
    fn only_the_chosen_accounts_are_laid_out() {
        let running = vec![("a".to_string(), 1), ("b".to_string(), 2), ("c".to_string(), 3)];
        assert_eq!(pids_for(&running, &[]), vec![1, 2, 3]);
        assert_eq!(pids_for(&running, &["c".to_string(), "a".to_string()]), vec![1, 3]);
        assert_eq!(pids_for(&running, &["gone".to_string()]), Vec::<u32>::new());
    }

    #[test]
    fn options_arrive_from_the_page_in_camel_case() {
        let o: LayoutOptions = serde_json::from_str(r#"{"w":480,"h":270,"corner":2,"gap":4,"ids":["x"],"append":true}"#).unwrap();
        assert_eq!(o, LayoutOptions { w: 480, h: 270, corner: 2, gap: 4, ids: vec!["x".into()], append: true });
        let empty: LayoutOptions = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, LayoutOptions::default());
    }

    #[test]
    fn previews_are_small_and_quiet() {
        assert_eq!(preview_command(77), "preview|77|160|40");
    }

    #[test]
    fn a_missing_picture_is_not_a_picture() {
        assert_eq!(preview_data_url("MIN"), None);
        assert_eq!(preview_data_url("BLANK\n"), None);
        assert_eq!(preview_data_url(""), None);
        assert_eq!(
            preview_data_url("/9j/4AAQ").as_deref(),
            Some("data:image/jpeg;base64,/9j/4AAQ")
        );
    }
}
