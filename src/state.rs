// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::logger;

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Settings {
    pub player_channel: String,
    pub studio_channel: String,
    pub roblox_domain: String,
    pub update_roblox: bool,
    pub static_directory: bool,
    pub max_thread_download: usize,
    /// Roblox client theme: "default" | "dark" | "light"
    pub theme: String,
    /// Keep the Roblox client running in the system tray after closing
    pub background_app: bool,
    /// Delete logs and caches older than this many days after Roblox exits; 0 disables
    pub cleaner_days: u32,
    /// Reaction when a launch URI asks for another channel:
    /// "automatic" (switch), "prompt" (ask user), "ignore" (keep current)
    pub channel_change_mode: String,
    /// Rejoin the last experience automatically after a client disconnect
    pub auto_rejoin: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ChannelChangeMode {
    Automatic,
    Prompt,
    Ignore,
}

impl Settings {
    pub fn channel_mode(&self) -> ChannelChangeMode {
        match self.channel_change_mode.to_ascii_lowercase().as_str() {
            "prompt" => ChannelChangeMode::Prompt,
            "ignore" => ChannelChangeMode::Ignore,
            _ => ChannelChangeMode::Automatic,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            player_channel: crate::paths::DEFAULT_CHANNEL.to_string(),
            studio_channel: crate::paths::DEFAULT_CHANNEL.to_string(),
            roblox_domain: crate::paths::ROBLOX_DOMAIN.to_string(),
            update_roblox: true,
            static_directory: false,
            max_thread_download: 8,
            theme: "default".to_string(),
            background_app: false,
            cleaner_days: 0,
            channel_change_mode: "automatic".to_string(),
            auto_rejoin: false,
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct State {
    pub is_first_launch: bool,
    pub force_reinstall: bool,
    /// Last place join URI, kept for the auto-rejoin feature.
    pub last_place_uri: String,
}

impl Default for State {
    fn default() -> Self {
        State {
            is_first_launch: true,
            force_reinstall: false,
            last_place_uri: String::new(),
        }
    }
}

/// Persisted per-binary (player/studio) install state.
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct DistributionState {
    pub version_guid: String,
    pub package_hashes: HashMap<String, String>,
    pub mod_manifest: Vec<String>,
}

pub fn load<T: DeserializeOwned + Default>(path: &Path) -> T {
    match std::fs::read_to_string(path) {
        Ok(contents) => match serde_json::from_str(&contents) {
            Ok(value) => value,
            Err(e) => {
                logger::error(&format!(
                    "Failed to parse {}: {e}; using defaults",
                    path.display()
                ));
                T::default()
            }
        },
        Err(_) => T::default(),
    }
}

pub fn save<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path must be absolute",
        ));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let contents = serde_json::to_string_pretty(value).map_err(io::Error::other)?;
    write_atomic(path, contents.as_bytes())
}

/// Writes via temp file + rename so a crash never leaves a truncated store behind.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temp: PathBuf = path.with_extension("tmp");
    {
        let mut file = std::fs::File::create(&temp)?;
        io::Write::write_all(&mut file, bytes)?;
        io::Write::flush(&mut file)?;
        file.sync_all().ok();
    }
    std::fs::rename(&temp, path)
}

/// A shared bundle of all persisted state handed through the bootstrapper.
pub struct Ctx {
    pub settings: Settings,
    pub state: State,
    pub player_state: DistributionState,
    pub studio_state: DistributionState,
    pub settings_path: PathBuf,
    pub state_path: PathBuf,
    pub player_state_path: PathBuf,
    pub studio_state_path: PathBuf,
}

impl Ctx {
    pub fn load(paths: &'static crate::paths::Paths) -> Ctx {
        Ctx {
            settings_path: paths.config_root.join("Settings.json"),
            state_path: paths.config_root.join("State.json"),
            player_state_path: paths.data_root.join("PlayerState.json"),
            studio_state_path: paths.data_root.join("StudioState.json"),
            settings: Default::default(),
            state: Default::default(),
            player_state: Default::default(),
            studio_state: Default::default(),
        }
        .with_loaded()
    }

    fn with_loaded(mut self) -> Ctx {
        self.settings = load(&self.settings_path);
        self.state = load(&self.state_path);
        self.player_state = load(&self.player_state_path);
        self.studio_state = load(&self.studio_state_path);
        self
    }

    pub fn save_settings(&self) {
        if let Err(e) = save(&self.settings_path, &self.settings) {
            logger::error(&format!("Failed to save Settings.json: {e}"));
        }
    }

    pub fn save_state(&self) {
        if let Err(e) = save(&self.state_path, &self.state) {
            logger::error(&format!("Failed to save State.json: {e}"));
        }
    }

    /// Player state on player launches, studio state otherwise.
    pub fn distribution_state(&self, is_studio: bool) -> &DistributionState {
        if is_studio {
            &self.studio_state
        } else {
            &self.player_state
        }
    }

    pub fn distribution_state_mut(&mut self, is_studio: bool) -> &mut DistributionState {
        if is_studio {
            &mut self.studio_state
        } else {
            &mut self.player_state
        }
    }

    pub fn save_distribution_state(&self, is_studio: bool) {
        let state = self.distribution_state(is_studio);
        let path = if is_studio {
            &self.studio_state_path
        } else {
            &self.player_state_path
        };
        if let Err(e) = save(path, state) {
            logger::error(&format!("Failed to save {}: {e}", path.display()));
        }
    }

    /// Player and Studio are installed through distinct packages, but both share the
    /// download cache, so cache cleanup must keep both known hash sets alive.
    pub fn all_cached_package_hashes(&self) -> Vec<&str> {
        self.player_state
            .package_hashes
            .values()
            .chain(self.studio_state.package_hashes.values())
            .map(String::as_str)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_roundtrip_pascal_case() {
        let settings = Settings::default();
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("\"PlayerChannel\""));
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.player_channel, settings.player_channel);

        let legacy: Settings = serde_json::from_str("{\"UpdateRoblox\": false}").unwrap();
        assert!(!legacy.update_roblox);
        assert_eq!(legacy.player_channel, "production");
    }

    #[test]
    fn channel_mode_parses() {
        let mut settings = Settings::default();
        assert_eq!(settings.channel_mode(), ChannelChangeMode::Automatic);
        settings.channel_change_mode = "Prompt".into();
        assert_eq!(settings.channel_mode(), ChannelChangeMode::Prompt);
        settings.channel_change_mode = "ignore".into();
        assert_eq!(settings.channel_mode(), ChannelChangeMode::Ignore);
        settings.channel_change_mode = "nonsense".into();
        assert_eq!(settings.channel_mode(), ChannelChangeMode::Automatic);
    }

    #[test]
    fn distribution_state_roundtrip() {
        let mut state = DistributionState {
            version_guid: "version-abc123".to_string(),
            ..Default::default()
        };
        state
            .package_hashes
            .insert("RobloxApp.zip".into(), "AAAA".into());
        let back: DistributionState =
            serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert_eq!(back.package_hashes["RobloxApp.zip"], "AAAA");
    }
}
