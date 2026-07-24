// Account instance screenshots, optionally cropped to a per-account outlined
// region, delivered to a Discord webhook. No files touch disk -- the native
// helper's "capture" command encodes straight to base64 over stdout (see
// RobloxNative.cs), so this module just decodes/re-encodes bytes in memory.
use crate::native::hide_window;
use crate::state::AppState;
use std::process::Stdio;
use std::time::Duration;
use tauri::AppHandle;

fn decode_capture_output(stdout: &str) -> Result<Vec<u8>, String> {
    let line = stdout
        .lines()
        .find(|l| l.starts_with("CAPTURED_B64:"))
        .ok_or_else(|| "No image data returned".to_string())?;
    let b64 = line["CAPTURED_B64:".len()..].trim();
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| e.to_string())
}

pub async fn capture_account_png(app: &AppHandle, state: &AppState, account_id: &str, region: Option<(f64, f64, f64, f64)>) -> Result<Vec<u8>, String> {
    let pid = state
        .account_pids
        .lock()
        .unwrap()
        .get(account_id)
        .copied()
        .ok_or_else(|| "No running instance for this account".to_string())?;
    let exe = crate::native::ensure_native_helper(app, state)
        .await
        .ok_or_else(|| "Native helper unavailable".to_string())?;

    let mut cmd = tokio::process::Command::new(&exe);
    cmd.arg("capture").arg(pid.to_string());
    if let Some((x, y, w, h)) = region {
        cmd.arg(x.to_string()).arg(y.to_string()).arg(w.to_string()).arg(h.to_string());
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    hide_window(&mut cmd);
    // Without this, a timeout below drops cmd.output()'s internal child
    // without killing it -- the process leaks in the background instead of
    // exiting, since nothing else ever holds a handle to it.
    cmd.kill_on_drop(true);

    let output = tokio::time::timeout(Duration::from_secs(15), cmd.output())
        .await
        .map_err(|_| "Capture timed out".to_string())?
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(if err.trim().is_empty() { "Capture failed".to_string() } else { err.trim().to_string() });
    }
    decode_capture_output(&String::from_utf8_lossy(&output.stdout))
}

pub async fn capture_preview_b64(app: &AppHandle, state: &AppState, account_id: &str) -> Result<String, String> {
    let png = capture_account_png(app, state, account_id, None).await?;
    use base64::Engine;
    Ok(base64::engine::general_purpose::STANDARD.encode(&png))
}

// Discord's multi-attachment webhook format: each file gets its own part
// named files[0], files[1], etc, all in one POST -- one message per capture
// pass instead of spamming a separate message per outlined spot.
pub async fn send_to_discord_webhook(state: &AppState, webhook_url: &str, images: Vec<Vec<u8>>, content: &str) -> Result<(), String> {
    let mut form = reqwest::multipart::Form::new().text("content", content.to_string());
    for (i, image) in images.into_iter().enumerate() {
        let part = reqwest::multipart::Part::bytes(image)
            .file_name(format!("screenshot-{}.png", i + 1))
            .mime_str("image/png")
            .map_err(|e| e.to_string())?;
        form = form.part(format!("files[{}]", i), part);
    }
    let resp = state.http.post(webhook_url).multipart(form).send().await.map_err(|e| e.to_string())?;
    if resp.status().is_success() {
        Ok(())
    } else {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        Err(format!("Webhook returned {}: {}", status, body.chars().take(300).collect::<String>()))
    }
}

pub async fn capture_and_send(
    app: &AppHandle,
    state: &AppState,
    account_id: &str,
    username: &str,
    webhook_url: &str,
    regions: Vec<(f64, f64, f64, f64)>,
) -> Result<(), String> {
    let mut images = Vec::new();
    if regions.is_empty() {
        images.push(capture_account_png(app, state, account_id, None).await?);
    } else {
        for region in regions {
            images.push(capture_account_png(app, state, account_id, Some(region)).await?);
        }
    }
    let content = format!("**{}** \u{2014} {}", username, chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"));
    send_to_discord_webhook(state, webhook_url, images, &content).await
}
