// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

use std::path::{Path, PathBuf};

use crate::logger;
use crate::paths::{self, Paths};
use crate::sober;

/// Location of Roblox's user settings (UserGameSettings), mirroring
/// Froststrap's GBSEditor.FileLocation.
pub fn file_location(paths: &Paths) -> PathBuf {
    if cfg!(windows) {
        paths.roblox.join("GlobalBasicSettings_13.xml")
    } else {
        sober::base_settings_file(paths)
    }
}

/// A minimal editor for the UserGameSettings XML: edits happen on the raw
/// document text so every property this app does not know about round-trips
/// untouched. Element kinds follow Froststrap's GBSEditor.PresetPaths.
#[derive(Default)]
pub struct Gbs {
    pub raw: String,
}

impl Gbs {
    /// Loads the file, or falls back to a fresh template that Roblox will
    /// happily extend (absent properties simply get their default).
    pub fn load(paths: &Paths) -> Gbs {
        let path = file_location(paths);
        match std::fs::read_to_string(&path) {
            Ok(raw) => {
                if raw.contains("<Item class=\"UserGameSettings\">") {
                    return Gbs { raw };
                }
                logger::error(&format!(
                    "{} is not a UserGameSettings document; using a template",
                    path.display()
                ));
                Gbs::template()
            }
            Err(_) => Gbs::template(),
        }
    }

    fn template() -> Gbs {
        Gbs {
            raw: "<roblox version=\"4\">\r\n  <Item class=\"UserGameSettings\" referent=\"RBX0\">\r\n    <Properties>\r\n    </Properties>\r\n  </Item>\r\n</roblox>\r\n"
                .to_string(),
        }
    }

    /// Raw text of a scalar setting, or `None` when the element is missing.
    pub fn get(&self, kind: &str, name: &str) -> Option<String> {
        let open = format!("<{kind} name=\"{name}\">");
        let pos = self.raw.find(&open)?;
        let start = pos + open.len();
        let close = format!("</{kind}>");
        let end = start + self.raw[start..].find(&close)?;
        Some(self.raw[start..end].to_string())
    }

    pub fn get_bool(&self, name: &str) -> Option<bool> {
        self.get("bool", name)
            .map(|v| v.trim().eq_ignore_ascii_case("true"))
    }

    pub fn get_float(&self, name: &str) -> Option<f64> {
        self.get("float", name)?
            .trim()
            .replace(',', ".")
            .parse()
            .ok()
    }

    /// Component of a Vector2 child element.
    pub fn get_vector(&self, name: &str, axis: &str) -> Option<String> {
        let open = format!("<Vector2 name=\"{name}\">");
        let pos = self.raw.find(&open)?;
        let start = pos + open.len();
        let end = start + self.raw[start..].find("</Vector2>")?;
        let scope = &self.raw[start..end];
        let axis_open = format!("<{axis}>");
        let axis_pos = scope.find(&axis_open)?;
        let axis_start = axis_pos + axis_open.len();
        let axis_close = format!("</{axis}>");
        let axis_end = axis_start + scope[axis_start..].find(&axis_close)?;
        Some(scope[axis_start..axis_end].to_string())
    }

    /// Writes a scalar setting, creating the element inside the
    /// UserGameSettings Properties block when missing.
    pub fn set(&mut self, kind: &str, name: &str, value: &str) -> Result<(), String> {
        let open = format!("<{kind} name=\"{name}\">");
        if let Some(pos) = self.raw.find(&open) {
            let start = pos + open.len();
            let close = format!("</{kind}>");
            let end = start
                + self.raw[start..]
                    .find(&close)
                    .ok_or_else(|| format!("unclosed element {kind} {name}"))?;
            self.raw.replace_range(start..end, value);
            return Ok(());
        }
        if !KNOWN_TYPES.contains(&kind) {
            return Err(format!("cannot create element of type {kind}"));
        }
        let props_end = self
            .raw
            .find("</Properties>")
            .ok_or("document has no UserGameSettings Properties block")?;
        let element = format!("\r\n      {open}{value}</{kind}>");
        self.raw.insert_str(props_end, &element);
        Ok(())
    }

    pub fn set_bool(&mut self, name: &str, value: bool) -> Result<(), String> {
        self.set("bool", name, if value { "true" } else { "false" })
    }

    pub fn set_float(&mut self, name: &str, value: f64) -> Result<(), String> {
        self.set("float", name, &format!("{value}"))
    }

    /// Creates the Vector2 element (X=0, Y=0) when missing, like Froststrap's
    /// CreateMissingElement.
    pub fn ensure_vector(&mut self, name: &str) -> Result<(), String> {
        if self.raw.contains(&format!("<Vector2 name=\"{name}\">")) {
            return Ok(());
        }
        let props_end = self
            .raw
            .find("</Properties>")
            .ok_or("document has no UserGameSettings Properties block")?;
        let element = format!("\r\n      <Vector2 name=\"{name}\"><X>0</X><Y>0</Y></Vector2>");
        self.raw.insert_str(props_end, &element);
        Ok(())
    }

    pub fn set_vector(&mut self, name: &str, axis: &str, value: &str) -> Result<(), String> {
        let open = format!("<Vector2 name=\"{name}\">");
        let pos = self
            .raw
            .find(&open)
            .ok_or_else(|| format!("Vector2 {name} not found"))?;
        let start = pos + open.len();
        let end = start
            + self.raw[start..]
                .find("</Vector2>")
                .ok_or_else(|| format!("unclosed Vector2 {name}"))?;
        let axis_open = format!("<{axis}>");
        let axis_close = format!("</{axis}>");
        let axis_pos = self.raw[start..end]
            .find(&axis_open)
            .ok_or_else(|| format!("Vector2 {name} has no {axis}"))?;
        let axis_start = start + axis_pos + axis_open.len();
        let axis_end = axis_start
            + self.raw[axis_start..end]
                .find(&axis_close)
                .ok_or_else(|| format!("Vector2 {name} has unclosed {axis}"))?;
        self.raw.replace_range(axis_start..axis_end, value);
        Ok(())
    }

    /// Persists the document atomically (temp file + rename), like Froststrap.
    pub fn save(&self, paths: &Paths) -> Result<(), String> {
        let path = file_location(paths);
        if !self.raw.contains("</Properties>") {
            return Err("document is missing its UserGameSettings Properties block".to_string());
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        let temp = path.with_extension("xml.tmp");
        std::fs::write(&temp, &self.raw).map_err(|e| format!("write {}: {e}", temp.display()))?;
        std::fs::rename(&temp, &path).map_err(|e| format!("{}: {e}", path.display()))?;
        logger::info(&format!("Saved {}", path.display()));
        Ok(())
    }

    /// Imports a UserGameSettings XML from `path`, replacing the current file.
    pub fn import(path: &Path) -> Result<(), String> {
        let contents =
            std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if !contents.contains("<Item class=\"UserGameSettings\">") {
            return Err(format!(
                "{} is not a UserGameSettings document",
                path.display()
            ));
        }
        let dest = file_location(paths::get());
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        std::fs::write(&dest, contents).map_err(|e| format!("write {}: {e}", dest.display()))?;
        logger::info(&format!("Imported Roblox settings from {}", path.display()));
        Ok(())
    }

    /// Exports the current document to `path`.
    pub fn export(&self, path: &Path) -> Result<(), String> {
        std::fs::write(path, &self.raw).map_err(|e| format!("write {}: {e}", path.display()))?;
        logger::info(&format!("Exported Roblox settings to {}", path.display()));
        Ok(())
    }

    /// Roblox overwrites this file on exit unless it is read-only
    /// (Froststrap's SetReadOnly), which is required for edited settings to
    /// actually stick.
    pub fn set_read_only(read_only: bool) -> Result<(), String> {
        let path = file_location(paths::get());
        if !path.exists() {
            return Ok(());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path)
                .map_err(|e| format!("stat {}: {e}", path.display()))?
                .permissions();
            perms.set_mode(if read_only { 0o444 } else { 0o600 });
            std::fs::set_permissions(&path, perms)
                .map_err(|e| format!("chmod {}: {e}", path.display()))?;
        }
        #[cfg(not(unix))]
        {
            let mut perms = std::fs::metadata(&path)
                .map_err(|e| format!("stat {}: {e}", path.display()))?
                .permissions();
            perms.set_readonly(read_only);
            std::fs::set_permissions(&path, perms)
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Ok(())
    }

    pub fn is_read_only() -> bool {
        file_location(paths::get())
            .metadata()
            .map(|m| m.permissions().readonly())
            .unwrap_or(false)
    }
}

/// rbx element types we are allowed to synthesize when a property is missing.
const KNOWN_TYPES: [&str; 5] = ["bool", "int", "float", "token", "string"];

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> Gbs {
        Gbs {
            raw: "<roblox version=\"4\">\r\n  <Item class=\"UserGameSettings\" referent=\"RBX0\">\r\n    <Properties>\r\n      <bool name=\"Fullscreen\">true</bool>\r\n      <float name=\"MasterVolume\">1</float>\r\n      <Vector2 name=\"MouseSensitivityFirstPerson\"><X>1</X><Y>1</Y></Vector2>\r\n    </Properties>\r\n  </Item>\r\n</roblox>\r\n"
                .to_string(),
        }
    }

    #[test]
    fn get_reads_scalar_and_vector() {
        let g = doc();
        assert_eq!(g.get("bool", "Fullscreen").as_deref(), Some("true"));
        assert_eq!(g.get_float("MasterVolume"), Some(1.0));
        assert_eq!(
            g.get_vector("MouseSensitivityFirstPerson", "X").as_deref(),
            Some("1")
        );
        assert_eq!(g.get("bool", "Missing"), None);
    }

    #[test]
    fn set_replaces_and_creates() {
        let mut g = doc();
        g.set_bool("Fullscreen", false).unwrap();
        assert_eq!(g.get("bool", "Fullscreen").as_deref(), Some("false"));
        g.set("int", "FramerateCap", "144").unwrap();
        assert_eq!(g.get("int", "FramerateCap").as_deref(), Some("144"));
        assert_eq!(g.get_bool("Fullscreen"), Some(false));
    }

    #[test]
    fn ensure_vector_then_update_axis() {
        let mut g = doc();
        g.ensure_vector("MouseSensitivityThirdPerson").unwrap();
        g.set_vector("MouseSensitivityThirdPerson", "Y", "2.5")
            .unwrap();
        assert_eq!(
            g.get_vector("MouseSensitivityThirdPerson", "Y").as_deref(),
            Some("2.5")
        );
        assert_eq!(
            g.get_vector("MouseSensitivityThirdPerson", "X").as_deref(),
            Some("0")
        );
    }
}
