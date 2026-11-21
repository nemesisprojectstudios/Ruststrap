// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

//! Windows-only registry management: protocol handlers, file associations, and the
//! channel keys Roblox consults. All operations target HKCU only (no elevation).

#[cfg(windows)]
use winreg::{enums::HKEY_CURRENT_USER, RegKey};

#[cfg(windows)]
const CLASSES_ROOT: &str = r"Software\Classes";

#[cfg(windows)]
fn register_protocol(handler: &std::path::Path, key: &str, name: &str, args: &str) {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(root) = hkcu.create_subkey(format!(r"{CLASSES_ROOT}\{key}")) else {
        crate::logger::error(&format!("Failed to create registry key for {key} protocol"));
        return;
    };
    let (root, _disposition) = root;

    let handler_str = handler.display().to_string();
    if root.get_value::<String, _>("").ok().is_none() {
        let _ = root.set_value("", &format!("URL: {name} Protocol"));
        let _ = root.set_value("URL Protocol", &"");
    }

    let command = format!("\"{handler_str}\" {args}");
    let Ok((icon, _)) = root.create_subkey("DefaultIcon") else { return };
    let Ok((command_key, _)) = root.create_subkey(r"shell\open\command") else { return };
    if command_key.get_value::<String, _>("").ok().as_deref() != Some(command.as_str()) {
        let _ = icon.set_value("", &handler_str);
        let _ = command_key.set_value("", &command);
    }
}

#[cfg(windows)]
pub fn register_player(executable: &std::path::Path) {
    register_protocol(executable, "roblox", "Roblox", r#"--player "%1""#);
    register_protocol(executable, "roblox-player", "Roblox", r#"--player "%1""#);
}

#[cfg(windows)]
pub fn register_studio(executable: &std::path::Path) {
    register_protocol(executable, "roblox-studio", "Roblox", r#"--studio "%1""#);
    register_protocol(executable, "roblox-studio-auth", "Roblox", r#"--studio "%1""#);

    for ext in [".rbxl", ".rbxlx", ".rbxm", ".rbxmx"] {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let Ok((key, _)) = hkcu
            .create_subkey(format!(r"{CLASSES_ROOT}\{ext}"))
            
        else {
            continue;
        };
        let _ = key.create_subkey(r"Roblox.Place\ShellNew");
        let _ = key.set_value("", &"Roblox.Place");
    }

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok((key, _)) = hkcu.create_subkey(format!(r"{CLASSES_ROOT}\Roblox.Place")) else {
        return;
    };
    let _ = key.set_value("", &"Roblox Place");
    if let Ok((open, _)) = key.create_subkey(r"shell\Open") {
        let _ = open.set_value("", &"Open");
        if let Ok((command_key, _)) = open.create_subkey("command") {
            let command = format!("\"{}\" --studio \"%1\"", executable.display());
            let _ = command_key.set_value("", &command);
        }
    }
}

#[cfg(windows)]
pub fn register_client_location(is_studio: bool, client_path: &str) {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok((key, _)) = hkcu.create_subkey(r"Software\Ruststrap") else {
        return;
    };
    let name = if is_studio { "StudioPath" } else { "PlayerPath" };
    let _ = key.set_value(name, &client_path);
}

#[cfg(windows)]
pub fn sync_channel(is_studio: bool, domain: &str, channel: &str, is_default: bool) {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let registry_name = if is_studio { "RobloxStudio" } else { "RobloxPlayer" };
    let Ok((key, _)) = hkcu
        .create_subkey(format!(r"Software\ROBLOX Corporation\Environments\{registry_name}\Channel"))
        
    else {
        return;
    };
    let value = if is_default { "" } else { channel };
    let _ = key.set_value(format!("www.{domain}"), &value);
}

#[cfg(windows)]
pub fn unregister_all() {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    for key in ["roblox", "roblox-player", "roblox-studio", "roblox-studio-auth"] {
        let _ = hkcu.delete_subkey_all(format!(r"{CLASSES_ROOT}\{key}"));
    }
}
