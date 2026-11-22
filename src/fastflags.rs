// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::logger;
use crate::paths::Paths;

/// Community-maintained Roblox FastFlag allowlist (September 2025). Flags
/// outside it may be ignored outright by the client, so the editor marks them.
pub const ALLOWLIST: [&str; 18] = [
    "DFIntCSGLevelOfDetailSwitchingDistance",
    "DFIntCSGLevelOfDetailSwitchingDistanceL12",
    "DFIntCSGLevelOfDetailSwitchingDistanceL23",
    "DFIntCSGLevelOfDetailSwitchingDistanceL34",
    "DFFlagDebugPauseVoxelizer",
    "DFFlagDisableDPIScale",
    "DFFlagTextureQualityOverrideEnabled",
    "DFIntDebugFRMQualityLevelOverride",
    "DFIntTextureQualityOverride",
    "FFlagDebugGraphicsPreferD3D11",
    "FFlagDebugGraphicsPreferOpenGL",
    "FFlagDebugGraphicsPreferVulkan",
    "FFlagDebugSkyGray",
    "FFlagHandleAltEnterFullscreenManually",
    "FIntDebugForceMSAASamples",
    "FIntFRMMaxGrassDistance",
    "FIntFRMMinGrassDistance",
    "FIntGrassMovementReducedMotionFactor",
];

/// The community-standard flag that disables the screenshots/video capture
/// hotkeys. Not on the current allowlist, so the preset calling it is
/// labeled best-effort in the UI.
pub const CAPTURE_DISABLE_FLAG: &str = "FFlagEnableCapturesHotkeyExperiment_v4";

pub fn is_allowlisted(name: &str) -> bool {
    ALLOWLIST.contains(&name)
}

/// Froststrap-compatible flag profile storage: one JSON flag map per file,
/// named after the profile, under SavedFlagProfiles in the config root.
fn profiles_dir(paths: &Paths) -> PathBuf {
    paths.config_root.join("SavedFlagProfiles")
}

fn sanitize_profile_name(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.starts_with('.') {
        return None;
    }
    if trimmed
        .chars()
        .any(|c| {
            matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control()
        })
    {
        return None;
    }
    Some(trimmed.to_string())
}

/// Sorted list of saved profile names.
pub fn list_profiles(paths: &Paths) -> Vec<String> {
    let mut names = Vec::new();
    let Ok(entries) = std::fs::read_dir(profiles_dir(paths)) else {
        return names;
    };
    for entry in entries.flatten() {
        if entry.path().is_file() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort_by_key(|name| name.to_lowercase());
    names
}

/// Saves a flag map under the given profile name; returns the stored name.
pub fn save_profile(
    paths: &Paths,
    name: &str,
    flags: &Map<String, Value>,
) -> Result<String, String> {
    let name = sanitize_profile_name(name)
        .ok_or_else(|| "profile name is empty or contains forbidden characters".to_string())?;
    let path = profiles_dir(paths).join(&name);
    save(&path, flags).map_err(|e| format!("failed to save profile {name}: {e}"))?;
    Ok(name)
}

pub fn load_profile(paths: &Paths, name: &str) -> Result<Map<String, Value>, String> {
    let name =
        sanitize_profile_name(name).ok_or_else(|| "invalid profile name".to_string())?;
    read_flags(&profiles_dir(paths).join(&name)).ok_or_else(|| {
        format!("profile {name} is missing or not a JSON object of flags")
    })
}

pub fn delete_profile(paths: &Paths, name: &str) -> Result<(), String> {
    let name =
        sanitize_profile_name(name).ok_or_else(|| "invalid profile name".to_string())?;
    std::fs::remove_file(profiles_dir(paths).join(&name))
        .map_err(|e| format!("failed to delete profile {name}: {e}"))
}

fn read_flags(path: &Path) -> Option<Map<String, Value>> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
}

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
    read_flags(&path).unwrap_or_else(|| {
        logger::error(&format!(
            "Fast flags file {} is not a valid JSON object; ignoring it",
            path.display()
        ));
        Map::new()
    })
}

pub fn save(path: &Path, flags: &Map<String, Value>) -> Result<(), std::io::Error> {
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
    if let Err(e) = std::fs::create_dir_all(&dest_dir)
        .and_then(|()| std::fs::copy(&source, dest_dir.join("ClientAppSettings.json")).map(|_| ()))
    {
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

/// Normalizes any JSON value into the string form the Windows flag file uses.
pub fn flag_value_to_string(value: &Value) -> String {
    match value {
        Value::String(inner) => inner.clone(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_membership_is_exact() {
        assert!(is_allowlisted("FFlagDebugSkyGray"));
        assert!(!is_allowlisted("fflagdebugskygray"));
        assert!(!is_allowlisted("FFlagDebugSkyGrayExtra"));
        assert!(!is_allowlisted(""));
        assert_eq!(ALLOWLIST.len(), 18);
    }

    #[test]
    fn profile_names_are_sanitized() {
        let base = std::env::temp_dir().join(format!(
            "ruststrap-test-profiles-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let paths = crate::paths::Paths {
            config_root: base.clone(),
            data_root: base.clone(),
            downloads: base.clone(),
            versions: base.clone(),
            modifications: base.clone(),
            logs: base.clone(),
            roblox: base,
        };
        let mut flags = Map::new();
        flags.insert("FFlagDebugSkyGray".into(), Value::String("True".into()));

        let stored = save_profile(&paths, "My Profile", &flags).unwrap();
        assert_eq!(stored, "My Profile");
        assert_eq!(load_profile(&paths, "My Profile").unwrap(), flags);
        assert_eq!(list_profiles(&paths), vec!["My Profile".to_string()]);

        assert!(save_profile(&paths, "a/b", &flags).is_err());
        assert!(save_profile(&paths, "..", &flags).is_err());
        assert!(save_profile(&paths, "  ", &flags).is_err());
        assert!(!paths
            .config_root
            .join("SavedFlagProfiles")
            .join("a/b")
            .exists());

        delete_profile(&paths, "My Profile").unwrap();
        assert!(list_profiles(&paths).is_empty());
        assert!(load_profile(&paths, "My Profile").is_err());
        let _ = std::fs::remove_dir_all(&paths.config_root);
    }

    #[test]
    fn flag_values_normalize_to_strings() {
        assert_eq!(flag_value_to_string(&Value::Bool(true)), "True");
        assert_eq!(flag_value_to_string(&json_bool()), "False");
        assert_eq!(flag_value_to_string(&serde_json::json!("abc")), "abc");
        assert_eq!(flag_value_to_string(&serde_json::json!(120)), "120");
    }

    fn json_bool() -> Value {
        Value::Bool(false)
    }

    #[test]
    fn coercion_types() {
        assert_eq!(coerce_flag("True"), Value::Bool(true));
        assert_eq!(coerce_flag("false"), Value::Bool(false));
        assert_eq!(coerce_flag("120"), Value::Number(120.into()));
        assert_eq!(
            coerce_flag("1.5"),
            serde_json::from_str::<Value>("1.5").unwrap()
        );
        assert_eq!(coerce_flag("level8"), Value::String("level8".to_string()));
    }
}
