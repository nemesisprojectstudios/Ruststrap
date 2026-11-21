// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::logger;
use crate::paths::Paths;

/// Canonical user-editable fast flag source (same file Froststrap uses).
pub fn fflags_path(paths: &Paths) -> PathBuf {
    paths
        .modifications
        .join("ClientSettings")
        .join("ClientAppSettings.json")
}

/// Loads user flags, creating the file with Froststrap's shipped default on first run.
pub fn load_or_init(paths: &Paths) -> Map<String, Value> {
    let path = fflags_path(paths);
    if !path.exists() {
        let mut flags = Map::new();
        flags.insert(
            "FFlagHandleAltEnterFullscreenManually".to_string(),
            Value::String("False".to_string()),
        );
        if let Err(e) = save(&path, &flags) {
            logger::error(&format!("Failed to create default fast flags file: {e}"));
        }
        return flags;
    }
    match std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
    {
        Some(flags) => flags,
        None => {
            logger::error(&format!(
                "Fast flags file {} is not a valid JSON object; ignoring it",
                path.display()
            ));
            Map::new()
        }
    }
}

fn save(path: &Path, flags: &Map<String, Value>) -> Result<(), std::io::Error> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let contents = serde_json::to_string_pretty(flags).map_err(std::io::Error::other)?;
    std::fs::write(path, contents)
}

/// Write the flag source into a version directory so the client picks it up.
pub fn apply_to_version_dir(paths: &Paths, version_dir: &Path) {
    let source = fflags_path(paths);
    if !source.exists() {
        return;
    }
    let dest_dir = version_dir.join("ClientSettings");
    if let Err(e) = std::fs::create_dir_all(&dest_dir).and_then(|()| {
        std::fs::copy(&source, dest_dir.join("ClientAppSettings.json")).map(|_| ())
    }) {
        logger::error(&format!("Failed to install fast flags: {e}"));
    }
}

/// Sober keeps sandbox-typed values (bool/int/float) rather than strings.
pub fn coerce_flag(value: &str) -> Value {
    if value.eq_ignore_ascii_case("true") {
        return Value::Bool(true);
    }
    if value.eq_ignore_ascii_case("false") {
        return Value::Bool(false);
    }
    if let Ok(i) = value.parse::<i64>() {
        return Value::Number(i.into());
    }
    if let Ok(f) = value.parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return Value::Number(n);
        }
    }
    Value::String(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coercion_types() {
        assert_eq!(coerce_flag("True"), Value::Bool(true));
        assert_eq!(coerce_flag("false"), Value::Bool(false));
        assert_eq!(coerce_flag("120"), Value::Number(120.into()));
        assert_eq!(coerce_flag("1.5"), serde_json::from_str::<Value>("1.5").unwrap());
        assert_eq!(
            coerce_flag("level8"),
            Value::String("level8".to_string())
        );
    }
}
