// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::logger;
use crate::paths::Paths;

/// Location of Roblox's client storage file; Ruststrap only touches specific key
/// values in it, like the stock bootstrapper does.
fn app_storage_path(paths: &Paths) -> PathBuf {
    if cfg!(windows) {
        paths.roblox.join("LocalStorage").join("appStorage.json")
    } else {
        paths
            .roblox
            .join("data/sober/appData/LocalStorage/appStorage.json")
    }
}

/// Reapplied on every launch and after Roblox exits, because the client can reset it.
pub fn apply(paths: &Paths, settings: &crate::state::Settings) {
    let path = app_storage_path(paths);
    mutate(&path, |storage| {
        let background = if settings.background_app {
            "true"
        } else {
            "false"
        };
        // values in this file are strings, not booleans
        storage.insert("LaunchAtStartup".into(), Value::String(background.into()));
        storage.insert("MinimizeToTray".into(), Value::String(background.into()));
        apply_theme(storage, &settings.theme);
    });
}

fn apply_theme(storage: &mut Map<String, Value>, theme: &str) {
    let value = match theme {
        "dark" | "Dark" => "dark",
        "light" | "Light" => "light",
        _ => return,
    };
    storage.insert("AuthenticatedTheme".into(), Value::String(value.into()));

    // theme is also stored per account id inside a JSON-encoded string
    let Some(Value::String(user_id)) = storage.get("UserId") else {
        return;
    };
    if user_id.is_empty() {
        return;
    }
    let mut map: Map<String, Value> = storage
        .get("DeviceLevelTheme")
        .and_then(Value::as_str)
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    map.insert(user_id.clone(), Value::String(value.into()));
    match serde_json::to_string(&map) {
        Ok(encoded) => {
            storage.insert("DeviceLevelTheme".into(), Value::String(encoded));
        }
        Err(e) => logger::error(&format!("Failed to update per-account theme cache: {e}")),
    }
}

fn mutate(path: &Path, f: impl FnOnce(&mut Map<String, Value>)) {
    let mut storage: Map<String, Value> = match std::fs::read_to_string(path) {
        Ok(contents) => match serde_json::from_str(&contents) {
            Ok(Value::Object(map)) => map,
            Ok(_) => {
                logger::error(&format!(
                    "{} has unexpected contents, leaving it untouched",
                    path.display()
                ));
                return;
            }
            Err(e) => {
                logger::error(&format!(
                    "Failed to parse {}: {e}; leaving it untouched",
                    path.display()
                ));
                return;
            }
        },
        // an absent file only ever holds Ruststrap's own defaults, so create it
        Err(_) => Map::new(),
    };

    f(&mut storage);

    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match serde_json::to_string_pretty(&Value::Object(storage)) {
        Ok(contents) => {
            if let Err(e) = std::fs::write(path, contents) {
                logger::error(&format!("Failed to write {}: {e}", path.display()));
            }
        }
        Err(e) => logger::error(&format!("Failed to serialize app storage: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_updates_map_and_key() {
        let mut storage = Map::new();
        storage.insert("UserId".into(), Value::String("321".into()));
        storage.insert(
            "DeviceLevelTheme".into(),
            Value::String(r#"{"42":"dark"}"#.into()),
        );
        apply_theme(&mut storage, "light");
        assert_eq!(storage["AuthenticatedTheme"], Value::String("light".into()));
        let map: Map<String, Value> =
            serde_json::from_str(storage["DeviceLevelTheme"].as_str().unwrap()).unwrap();
        assert_eq!(map["42"], Value::String("dark".into()));
        assert_eq!(map["321"], Value::String("light".into()));
    }

    #[test]
    fn default_theme_leaves_storage() {
        let mut storage = Map::new();
        apply_theme(&mut storage, "default");
        assert!(storage.get("AuthenticatedTheme").is_none());
    }
}
