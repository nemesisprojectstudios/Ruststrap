// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::{Map, Value};

use crate::logger;
use crate::paths::{home_dir, Paths, SOBER_FLATPAK_ID};

/// Sober's flatpak data root. All Sober state (config.json, GlobalBasicSettings,
/// internal data) lives here and Ruststrap edits it in place.
pub fn data_dir(_paths: &Paths) -> PathBuf {
    home_dir().join(".var/app").join(SOBER_FLATPAK_ID)
}

pub fn config_file(paths: &Paths) -> PathBuf {
    data_dir(paths).join("config/sober/config.json")
}

fn flatpak_ok(args: &[&str]) -> Result<bool, String> {
    match Command::new("flatpak").args(args).output() {
        Ok(output) => Ok(output.status.success()),
        Err(e) => Err(format!("cannot run flatpak: {e}")),
    }
}

pub fn is_installed() -> bool {
    flatpak_ok(&["info", SOBER_FLATPAK_ID]).unwrap_or(false)
}

/// Verifies flatpak works and Sober is installed, installing it from Flathub if missing.
pub fn ensure_installed() -> Result<(), String> {
    logger::info("Checking Flatpak installation");
    match flatpak_ok(&["--version"]) {
        Ok(true) => {}
        Ok(false) => return Err("Flatpak is broken or misconfigured".to_string()),
        Err(_) => {
            return Err("Flatpak is required on Linux; install Flatpak and try again".to_string())
        }
    }

    if is_installed() {
        logger::info("Sober is already installed");
        return Ok(());
    }

    logger::info("Installing Sober from Flathub (may take a while)");
    let status = Command::new("flatpak")
        .args(["install", "--noninteractive", "flathub", SOBER_FLATPAK_ID])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| format!("cannot run flatpak: {e}"))?;
    if !status.success() {
        return Err(format!("flatpak install {SOBER_FLATPAK_ID} failed"));
    }
    logger::info("Sober installed");
    Ok(())
}

/// Updates an existing Sober flatpak to the latest Flathub build.
pub fn update() -> Result<(), String> {
    logger::info("Updating Sober via flatpak (may take a while)");
    let status = Command::new("flatpak")
        .args(["update", "--noninteractive", SOBER_FLATPAK_ID])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| format!("cannot run flatpak: {e}"))?;
    if !status.success() {
        return Err(format!("flatpak update {SOBER_FLATPAK_ID} failed"));
    }
    logger::info("Sober updated");
    Ok(())
}

/// Sober's Roblox user settings (GlobalBasicSettings_13.xml).
pub fn base_settings_file(paths: &Paths) -> PathBuf {
    data_dir(paths).join("data/sober/appData/GlobalBasicSettings_13.xml")
}

/// Strips `//` and `/* */` comments outside JSON strings, like Froststrap's
/// `ReadCommentHandling.Skip` for the same file.
fn strip_json_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' => match chars.peek() {
                Some('/') => {
                    for n in chars.by_ref() {
                        if n == '\n' {
                            out.push('\n');
                            break;
                        }
                    }
                }
                Some('*') => {
                    chars.next();
                    while let Some(n) = chars.next() {
                        if n == '*' && chars.peek() == Some(&'/') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => out.push('/'),
            },
            _ => out.push(c),
        }
    }
    out
}

/// Drops commas directly before `}` / `]`, like Froststrap's `AllowTrailingCommas`.
fn strip_trailing_commas(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            index += 1;
            continue;
        }
        if c == ',' {
            let mut next = index + 1;
            while next < chars.len() && chars[next].is_whitespace() {
                next += 1;
            }
            if next < chars.len() && (chars[next] == '}' || chars[next] == ']') {
                index += 1;
                continue;
            }
        }
        out.push(c);
        index += 1;
    }
    out
}

/// Parses Sober's config.json leniently (comments and trailing commas allowed).
/// Returns the top-level object, or an error a user can act on.
pub fn parse_config(text: &str) -> Result<Map<String, Value>, String> {
    let stripped = strip_json_comments(text);
    let as_object = |v: Value| v.as_object().cloned().unwrap_or_default();
    if let Ok(value) = serde_json::from_str::<Value>(&stripped) {
        return Ok(as_object(value));
    }
    serde_json::from_str::<Value>(&strip_trailing_commas(&stripped))
        .map(as_object)
        .map_err(|e| e.to_string())
}

/// Loads Sober's config.json as a JSON object (empty when missing or unreadable).
/// This is the single source of truth for fast flags and Sober's own options on Linux.
pub fn load_config(paths: &Paths) -> Map<String, Value> {
    let path = config_file(paths);
    match std::fs::read_to_string(&path) {
        Ok(contents) => match parse_config(&contents) {
            Ok(config) => config,
            Err(e) => {
                logger::error(&format!(
                    "{} is not valid JSON ({}); using defaults",
                    path.display(),
                    e
                ));
                Map::new()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            logger::info(&format!(
                "No Sober config at {} yet; using defaults",
                path.display()
            ));
            Map::new()
        }
        Err(e) => {
            logger::error(&format!(
                "Cannot read {}: {e}; using defaults",
                path.display()
            ));
            Map::new()
        }
    }
}

/// Writes Sober's config.json, preserving any keys the caller left untouched.
pub fn save_config(paths: &Paths, config: &Map<String, Value>) -> Result<(), String> {
    let path = config_file(paths);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let contents =
        serde_json::to_string_pretty(config).map_err(|e| format!("serialize Sober config: {e}"))?;
    std::fs::write(&path, contents).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

/// Reads a Sober config key, accepting typed or string-encoded booleans.
pub fn config_bool(config: &Map<String, Value>, key: &str) -> Option<bool> {
    match config.get(key) {
        Some(Value::Bool(b)) => Some(*b),
        Some(Value::String(s)) => Some(s.eq_ignore_ascii_case("true")),
        _ => None,
    }
}

pub fn kill() {
    logger::info("Killing any running Sober instance");
    let _ = Command::new("flatpak")
        .args(["kill", SOBER_FLATPAK_ID])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Launches Sober, translating the launch URI into its supported format.
pub fn launch(launch_uri: &str) -> Result<std::process::Child, String> {
    let uri = if launch_uri.is_empty() {
        "roblox://navigation/home".to_string()
    } else if launch_uri.starts_with("roblox://") {
        launch_uri.to_string()
    } else {
        let data = crate::gamejoin::get_join_data(launch_uri);
        crate::gamejoin::build_sober_uri(&data).ok_or_else(|| {
            format!(
                "cannot launch Sober: could not extract a joinable experience from {launch_uri}"
            )
        })?
    };

    logger::info(&format!("Launching Sober via flatpak: {uri}"));
    Command::new("flatpak")
        .args(["run", SOBER_FLATPAK_ID, &uri])
        .spawn()
        .map_err(|e| format!("failed to start Sober: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_config_tolerates_comments_and_trailing_commas() {
        let text = r#"{
            // line comment
            "fflags": {
                "FFlagExample": "True", // inline comment
                /* block
                   comment */
            },
            "dpi": "2",
        }"#;
        let config = parse_config(text).expect("lenient parse");
        assert_eq!(config["fflags"]["FFlagExample"], json!("True"));
        assert_eq!(config["dpi"], json!("2"));
    }

    #[test]
    fn parse_config_keeps_slashes_inside_strings() {
        let config = parse_config(r#"{"url": "https://example.com//path"}"#).expect("parse");
        assert_eq!(config["url"], json!("https://example.com//path"));
    }

    #[test]
    fn parse_config_reports_broken_json() {
        assert!(parse_config("{fflags:}").is_err());
    }
}
