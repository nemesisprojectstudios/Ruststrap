// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

use std::path::PathBuf;
use std::sync::OnceLock;

const NAME: &str = "Ruststrap";
pub const SOBER_FLATPAK_ID: &str = "org.vinegarhq.Sober";
pub const ROBLOX_DOMAIN: &str = "roblox.com";
pub const DEFAULT_CHANNEL: &str = "production";

pub struct Paths {
    pub config_root: PathBuf,
    pub data_root: PathBuf,
    pub downloads: PathBuf,
    pub versions: PathBuf,
    pub modifications: PathBuf,
    pub logs: PathBuf,
    /// Full Roblox instance storage: package downloads/version extraction on
    /// Windows, and Wine Studio installs on Linux. Sober manages its own
    /// state under ~/.var/app/org.vinegarhq.Sober (see sober.rs).
    pub roblox: PathBuf,
}

static PATHS: OnceLock<Paths> = OnceLock::new();

pub fn get() -> &'static Paths {
    PATHS.get().expect("paths not initialized")
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Resolves and creates all standard directories. Windows: %APPDATA%\Ruststrap (+ %LOCALAPPDATA%\Roblox).
/// Linux: XDG base directories.
pub fn init() -> &'static Paths {
    PATHS.get_or_init(|| {
        let (config_root, data_root, roblox, logs) = if cfg!(windows) {
            // defaulting to the current directory keeps the bootstrapper runnable
            // in stripped environments (sandboxed terminals etc.) without crashing
            let appdata = env_dir("APPDATA").unwrap_or_else(current_dir);
            let local = env_dir("LOCALAPPDATA").unwrap_or_else(current_dir);
            (
                appdata.join(NAME),
                appdata.join(NAME),
                local.join("Roblox"),
                appdata.join(NAME).join("Logs"),
            )
        } else {
            let home = env_dir("HOME").unwrap_or_else(|| PathBuf::from("."));
            let config = env_dir("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
            let data = env_dir("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local/share"));
            let state = env_dir("XDG_STATE_HOME").unwrap_or_else(|| home.join(".local/state"));
            (
                config.join(NAME),
                data.join(NAME),
                data.join(NAME).join("Versions").join("Sober"),
                state.join(NAME),
            )
        };

        let downloads = data_root.join("Downloads");
        let versions = data_root.join("Versions");
        let modifications = config_root.join("Modifications");

        for dir in [&config_root, &data_root, &downloads, &versions, &modifications, &logs] {
            let _ = std::fs::create_dir_all(dir);
        }

        Paths {
            config_root,
            data_root,
            downloads,
            versions,
            modifications,
            logs,
            roblox,
        }
    })
}

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

fn current_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}
