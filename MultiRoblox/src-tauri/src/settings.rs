use crate::paths::settings_path;
use serde_json::{Map, Value};

pub fn load_settings() -> Map<String, Value> {
    match std::fs::read_to_string(settings_path()) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => Map::new(),
    }
}

pub fn save_settings(s: &Map<String, Value>) {
    if let Ok(json) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(settings_path(), json);
    }
}

pub fn get_str(s: &Map<String, Value>, key: &str) -> Option<String> {
    s.get(key).and_then(|v| v.as_str()).map(|v| v.to_string())
}
