// Roblox FPS cap, stored as <int name="FramerateCap"> in
// %LOCALAPPDATA%\Roblox\GlobalBasicSettings_13.xml (the one method that still
// works since Roblox's FFlag allowlist).
//
// Editing: the element lives in the <Properties> of the UserGameSettings
// item. Only that block is touched, so every other setting in the file is
// left byte-for-byte as Roblox wrote it. The previous regex edit had three
// flaws this replaces: it only recognised non-negative integers, so a
// FramerateCap of -1 (or a float) was invisible to it; it then "inserted" a
// second element right before the first </Item>, which is outside
// <Properties> and ignored by Roblox; and every later save repeated the
// insert. Stray elements left by that are cleaned up on the next write.
//
// Protection: Roblox rewrites this whole file from memory whenever a client
// exits or its in-game settings change, which silently reverts the cap the
// user set here. While the lock is on (default), a guard watches the file
// and puts the cap back whenever something else changed it, and every launch
// re-applies it right before spawning so the new client starts with it.
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tauri::AppHandle;

/// Written for "unlimited"; Roblox treats any cap this high as uncapped.
pub const UNLIMITED: i64 = 9999;
const DEFAULT_CAP: i64 = 60;
/// The cap the user chose in Vanta Manager (absent until they choose one).
pub const CAP_KEY: &str = "fpsCap";
/// Whether that cap is defended against overwrites (default on).
pub const LOCK_KEY: &str = "fpsCapLock";
const GUARD_INTERVAL: Duration = Duration::from_secs(3);

static ITEM_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?is)<Item\s[^>]*class\s*=\s*"UserGameSettings"[^>]*>"#).unwrap());
static PROPS_OPEN_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)<Properties\s*>").unwrap());
static PROPS_CLOSE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)</Properties\s*>").unwrap());
/// Any scalar element named FramerateCap, whatever type Roblox (or an older
/// tool) wrote it as, with an optional sign and fraction.
static CAP_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?is)<(int|int64|float|double|token|string)\s+name\s*=\s*"FramerateCap"\s*>\s*(-?\d+(?:\.\d+)?)\s*</\s*(?:int|int64|float|double|token|string)\s*>"#)
        .unwrap()
});

pub fn settings_path() -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join("AppData").join("Local"))
        })?;
    Some(local.join("Roblox").join("GlobalBasicSettings_13.xml"))
}

/// The cap a UI value stands for: 0 or less is unlimited; everything else is
/// rounded and kept within what Roblox accepts.
pub fn normalize(cap: f64) -> i64 {
    if !cap.is_finite() {
        return DEFAULT_CAP;
    }
    let n = cap.round() as i64;
    if n <= 0 {
        UNLIMITED
    } else {
        n.clamp(1, UNLIMITED)
    }
}

/// Byte range of the UserGameSettings <Properties> content.
fn properties_span(xml: &str) -> Option<(usize, usize)> {
    let item = ITEM_RE.find(xml)?;
    let open = PROPS_OPEN_RE.find_at(xml, item.end())?;
    let close = PROPS_CLOSE_RE.find_at(xml, open.end())?;
    Some((open.end(), close.start()))
}

/// The cap Roblox will read: the first FramerateCap inside the settings block.
pub fn read_cap_from(xml: &str) -> Option<i64> {
    let (start, end) = properties_span(xml)?;
    let caps = CAP_RE.captures(&xml[start..end])?;
    caps[2].parse::<f64>().ok().map(|v| v.round() as i64)
}

/// Returns `xml` with exactly one FramerateCap, set to `value`, inside the
/// UserGameSettings properties. Everything else is preserved as-is.
pub fn set_cap_in(xml: &str, value: i64) -> Result<String, String> {
    let (start, end) = properties_span(xml).ok_or("GlobalBasicSettings_13.xml has no UserGameSettings block. Launch Roblox once so it rebuilds the file.")?;
    let tag = format!(r#"<int name="FramerateCap">{value}</int>"#);
    let newline = if xml.contains("\r\n") { "\r\n" } else { "\n" };

    let props = &xml[start..end];
    let mut replaced = false;
    let mut body = String::with_capacity(props.len() + tag.len());
    let mut last = 0;
    for m in CAP_RE.find_iter(props) {
        body.push_str(&props[last..m.start()]);
        if !replaced {
            body.push_str(&tag);
            replaced = true;
        } else {
            // A duplicate: drop it along with the indentation and line it sat on.
            let trimmed = body.trim_end_matches([' ', '\t']).len();
            body.truncate(trimmed);
            if body.ends_with('\n') {
                body.pop();
                if body.ends_with('\r') {
                    body.pop();
                }
            }
        }
        last = m.end();
    }
    body.push_str(&props[last..]);

    if !replaced {
        // Indent like the sibling properties: the closing tag's indentation
        // plus one tab, which is how Roblox lays the file out.
        let closing_indent: String = props
            .rsplit('\n')
            .next()
            .unwrap_or("")
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let content = body.trim_end_matches([' ', '\t', '\r', '\n']).to_string();
        body = format!("{content}{newline}{closing_indent}\t{tag}{newline}{closing_indent}");
    }

    // Repair copies the old editor put outside <Properties> (just before
    // </Item>), which Roblox never reads and which would otherwise pile up.
    let before = strip_caps(&xml[..start], newline);
    let after = strip_caps(&xml[end..], newline);
    Ok(format!("{before}{body}{after}"))
}

fn strip_caps(s: &str, newline: &str) -> String {
    if !CAP_RE.is_match(s) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for m in CAP_RE.find_iter(s) {
        let mut seg = s[last..m.start()].to_string();
        let trimmed = seg.trim_end_matches([' ', '\t']).len();
        seg.truncate(trimmed);
        if seg.ends_with(newline) {
            seg.truncate(seg.len() - newline.len());
        }
        out.push_str(&seg);
        last = m.end();
    }
    out.push_str(&s[last..]);
    out
}

fn read_file() -> Result<(PathBuf, String), String> {
    let p = settings_path().ok_or("Could not locate %LOCALAPPDATA%")?;
    match std::fs::read(&p) {
        Ok(bytes) => {
            let text = String::from_utf8(bytes).map_err(|_| {
                "GlobalBasicSettings_13.xml is not valid UTF-8, so it was left alone.".to_string()
            })?;
            Ok((p, text))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err("GlobalBasicSettings_13.xml not found - launch Roblox once to create it.".into())
        }
        Err(e) => Err(format!("Could not read GlobalBasicSettings_13.xml: {e}")),
    }
}

/// Current cap in the file (60, Roblox's default, when absent or unreadable).
pub fn read() -> i64 {
    // 0 or a negative cap (Roblox and some tools write -1) is not a frame
    // rate the UI can show or round-trip; report Roblox's default instead.
    read_file()
        .ok()
        .and_then(|(_, xml)| read_cap_from(&xml))
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_CAP)
}

fn write_file(p: &PathBuf, xml: &str) -> Result<(), String> {
    // Some bootstrappers mark this file read-only for their own FPS
    // unlocker; clear it so our write isn't silently blocked.
    if let Ok(meta) = std::fs::metadata(p) {
        let mut perms = meta.permissions();
        if perms.readonly() {
            // Clears FILE_ATTRIBUTE_READONLY on Windows. clippy flags this
            // because on Unix it widens the mode to 0o666; this file only
            // exists on Windows, and it is rewritten right after.
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            let _ = std::fs::set_permissions(p, perms);
        }
    }
    // Atomic so a crash mid-write can't leave Roblox a truncated file (it
    // would reset every setting in it). A running client can hold the file
    // in a way that blocks the rename, so fall back to an in-place write.
    crate::jsonfile::write_atomic(p, xml)
        .or_else(|_| std::fs::write(p, xml))
        .map_err(|e| {
            format!(
                "Failed to write GlobalBasicSettings_13.xml (it may be locked by another app): {e}"
            )
        })
}

/// Writes `value` into the file if it isn't already there. Ok(true) when the
/// file changed.
// Serialises every read-modify-write of the file: a user's write and the
// guard's restore running at once could otherwise interleave and leave the
// previous cap in place.
static FILE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn apply(value: i64) -> Result<bool, String> {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (p, xml) = read_file()?;
    if read_cap_from(&xml) == Some(value) && CAP_RE.find_iter(&xml).count() == 1 {
        return Ok(false);
    }
    let new_xml = set_cap_in(&xml, value)?;
    write_file(&p, &new_xml)?;
    Ok(true)
}

/// Sets the cap and remembers it as the one to defend.
pub fn write(cap: f64) -> Result<i64, String> {
    let value = normalize(cap);
    // Remembered first: a guard pass between the file write and this would
    // otherwise still see the old cap as the wanted one and restore it.
    if let Err(e) = crate::settings::update(|s| {
        s.insert(CAP_KEY.into(), json!(value));
    }) {
        eprintln!("[fps] could not remember the cap: {e}");
    }
    // An account's own cap is in the file while its client starts; the
    // shared one lands when that ends.
    if OVERRIDE.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
        return Ok(value);
    }
    apply(value)?;
    Ok(value)
}

/// An account's own cap while its client is starting: (cap, what the file
/// held before, to put back when no shared cap is defended).
static OVERRIDE: std::sync::Mutex<Option<(i64, Option<i64>)>> = std::sync::Mutex::new(None);

/// Writes an account's own cap for the client about to start, and defends it
/// (instead of the shared one) until end_override.
pub fn begin_override(value: i64) -> Result<(), String> {
    {
        let mut o = OVERRIDE.lock().unwrap_or_else(|e| e.into_inner());
        match o.as_mut() {
            Some(cur) => cur.0 = value,
            None => {
                let prior = read_file().ok().and_then(|(_, xml)| read_cap_from(&xml));
                *o = Some((value, prior));
            }
        }
    }
    apply(value).map(|_| ())
}

/// Puts the shared cap back: the defended one, else whatever was there before.
pub fn end_override() {
    let Some((_, prior)) = OVERRIDE.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return;
    };
    let back = shared_wanted().or(prior);
    if let Some(v) = back {
        if let Err(e) = apply(v) {
            eprintln!("[fps] could not put the shared cap back: {e}");
        }
    }
}

/// The cap to defend: an account's own while its client starts, else the
/// shared one if the user set one and the lock is on.
fn wanted() -> Option<i64> {
    if let Some((v, _)) = *OVERRIDE.lock().unwrap_or_else(|e| e.into_inner()) {
        return Some(v);
    }
    shared_wanted()
}

fn shared_wanted() -> Option<i64> {
    if !crate::settings::get_bool(LOCK_KEY, true) {
        return None;
    }
    crate::settings::snapshot()
        .get(CAP_KEY)
        .and_then(Value::as_f64)
        .map(normalize)
}

/// Puts the defended cap back if something changed it. Ok(Some((was, now)))
/// when a restore happened.
pub fn enforce() -> Result<Option<(Option<i64>, i64)>, String> {
    let Some(value) = wanted() else {
        return Ok(None);
    };
    let (_, xml) = read_file()?;
    let was = read_cap_from(&xml);
    if apply(value)? {
        Ok(Some((was, value)))
    } else {
        Ok(None)
    }
}

/// Watches the file (by mtime and size, so an unchanged file is never read)
/// and restores the cap after Roblox or another tool overwrites it.
pub fn start_guard(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut seen: Option<(SystemTime, u64)> = None;
        loop {
            tokio::time::sleep(GUARD_INTERVAL).await;
            if wanted().is_none() {
                seen = None;
                continue;
            }
            let Some(p) = settings_path() else { continue };
            let Ok(meta) = std::fs::metadata(&p) else {
                continue;
            };
            let stamp = (
                meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                meta.len(),
            );
            if seen == Some(stamp) {
                continue;
            }
            let mut settled = true;
            match tokio::task::spawn_blocking(enforce).await {
                Ok(Ok(Some((was, now)))) => {
                    let was = was
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "missing".into());
                    crate::native::emit_log(
                        &app,
                        "info",
                        "system",
                        &format!(
                            "FPS cap was changed to {was} by Roblox or another app; restored {now}"
                        ),
                        Some(json!({ "was": was, "fpsCap": now })),
                    );
                }
                Ok(Err(e)) => {
                    // Typically the file was locked mid-write; leave `seen`
                    // alone so the next tick tries again.
                    eprintln!("[fps] guard: {e}");
                    settled = false;
                }
                _ => {}
            }
            if !settled {
                continue;
            }
            // Re-stat after our own write so it doesn't count as a change.
            seen = std::fs::metadata(&p)
                .ok()
                .map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "<roblox>\r\n\t<Item class=\"UserGameSettings\" referent=\"RBX1\">\r\n\t\t<Properties>\r\n\t\t\t<bool name=\"Fullscreen\">false</bool>\r\n\t\t\t<int name=\"FramerateCap\">60</int>\r\n\t\t\t<int name=\"GraphicsQualityLevel\">5</int>\r\n\t\t</Properties>\r\n\t</Item>\r\n</roblox>\r\n";

    #[test]
    fn reads_the_cap_inside_user_game_settings() {
        assert_eq!(read_cap_from(SAMPLE), Some(60));
        let neg = SAMPLE.replace(">60<", ">-1<");
        assert_eq!(
            read_cap_from(&neg),
            Some(-1),
            "negative values used to be invisible"
        );
        let float = SAMPLE.replace(
            "<int name=\"FramerateCap\">60</int>",
            "<float name=\"FramerateCap\">144.0</float>",
        );
        assert_eq!(read_cap_from(&float), Some(144));
    }

    #[test]
    fn replacing_touches_only_the_cap() {
        let out = set_cap_in(SAMPLE, 240).unwrap();
        assert_eq!(out, SAMPLE.replace(">60<", ">240<"));
    }

    #[test]
    fn missing_cap_is_inserted_inside_properties_with_matching_indent() {
        let without = SAMPLE.replace("\t\t\t<int name=\"FramerateCap\">60</int>\r\n", "");
        let out = set_cap_in(&without, 144).unwrap();
        assert!(out.contains("\t\t\t<int name=\"GraphicsQualityLevel\">5</int>\r\n\t\t\t<int name=\"FramerateCap\">144</int>\r\n\t\t</Properties>"), "{out}");
        assert_eq!(read_cap_from(&out), Some(144));
    }

    #[test]
    fn duplicates_and_strays_from_the_old_editor_are_removed() {
        let broken = SAMPLE
            .replace(
                "\t\t</Properties>",
                "\t\t\t<int name=\"FramerateCap\">30</int>\r\n\t\t</Properties>",
            )
            .replace(
                "\t</Item>",
                "\t\t<int name=\"FramerateCap\">999</int>\r\n</Item>",
            );
        let out = set_cap_in(&broken, 120).unwrap();
        assert_eq!(CAP_RE.find_iter(&out).count(), 1, "{out}");
        assert_eq!(read_cap_from(&out), Some(120));
        assert!(
            out.contains("</Properties>\r\n\t</Item>") || out.contains("</Properties>\r\n</Item>"),
            "{out}"
        );
    }

    #[test]
    fn a_file_without_the_settings_block_is_refused() {
        assert!(set_cap_in("<roblox></roblox>", 60).is_err());
    }

    #[test]
    fn values_are_normalised() {
        assert_eq!(normalize(0.0), UNLIMITED);
        assert_eq!(normalize(-5.0), UNLIMITED);
        assert_eq!(normalize(143.6), 144);
        assert_eq!(normalize(50_000.0), UNLIMITED);
        assert_eq!(normalize(f64::NAN), DEFAULT_CAP);
    }
}
