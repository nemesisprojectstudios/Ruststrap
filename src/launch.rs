// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{Duration, Instant, SystemTime};

use crate::logger;
use crate::paths::Paths;
use crate::state::Settings;

/// How long a freshly spawned client is watched for an immediate exit; a
/// client that survives this long is healthy (logs already go to its own
/// `%LOCALAPPDATA%/Roblox/Logs` files).
const EARLY_EXIT_WINDOW: Duration = Duration::from_secs(15);

/// Launches the game and returns a handle to wait on (None when the process
/// detached). The returned `Option<Child>` is None for Windows Store-style launches.
pub fn launch(
    paths: &'static Paths,
    is_studio: bool,
    version_dir: &Path,
    launch_args: &str,
) -> Result<Option<Child>, String> {
    if cfg!(windows) {
        let exe = version_dir.join(crate::packages::executable_name(is_studio));
        logger::info(&format!("Starting Roblox: {}", exe.display()));
        verify_version_configs(version_dir);
        verify_core_files(version_dir, is_studio);
        let mut command = std::process::Command::new(&exe);
        command.current_dir(version_dir);
        command.stdout(std::process::Stdio::null());
        command.stderr(std::process::Stdio::null());
        if !launch_args.is_empty() {
            command.arg(launch_args);
        } else if !is_studio {
            // the stock desktop shortcut launches the home screen the same way;
            // a bare zero-argument launch exits instantly with code 1
            command.arg("--app");
            logger::info("No launch arguments: opening the Roblox home screen (--app)");
        }
        match command.spawn() {
            Ok(mut child) => {
                let spawn_time = SystemTime::now();
                let deadline = Instant::now() + EARLY_EXIT_WINDOW;
                loop {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            let description = describe_exit_code(status.code());
                            logger::error(&format!(
                                "Roblox exited early: {description} (raw status: {status})"
                            ));
                            if status.code() == Some(0)
                                && crate::processes::image_running(
                                    crate::processes::roblox_player_image(),
                                )
                            {
                                logger::info(
                                    "Another Roblox client process is running; the new client likely handed its session over to it",
                                );
                            }
                            harvest_client_logs(paths, is_studio, spawn_time);
                            return Ok(None);
                        }
                        Ok(None) if Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(500));
                        }
                        Ok(None) => {
                            logger::info(&format!("Roblox is running (PID {})", child.id()));
                            return Ok(Some(child));
                        }
                        Err(e) => {
                            logger::warn(&format!("Could not poll Roblox: {e}"));
                            return Ok(Some(child));
                        }
                    }
                }
            }
            Err(e) => {
                // a broken executable gets removed so the next run reinstalls cleanly
                let _ = std::fs::remove_file(&exe);
                Err(format!("Failed to start Roblox: {e}"))
            }
        }
    } else if is_studio {
        launch_studio_via_wine(paths, version_dir, launch_args)
    } else {
        crate::sober::launch(launch_args).map(Some)
    }
}

/// Linux Studio runs the Windows binaries through Wine with a dedicated prefix.
/// Ruststrap relies on a system Wine install instead of shipping a Wine build.
fn launch_studio_via_wine(
    paths: &'static Paths,
    version_dir: &Path,
    launch_args: &str,
) -> Result<Option<Child>, String> {
    let wine = which_wine().ok_or_else(|| {
        "Wine is required to run Roblox Studio on Linux; install wine and try again".to_string()
    })?;

    let prefix = paths.data_root.join("Wine").join("prefixes").join("studio");
    if !prefix.join("drive_c/windows").exists() {
        logger::info("Initializing Wine prefix for Studio");
        let status = std::process::Command::new(&wine)
            .arg("-u")
            .env("WINEPREFIX", &prefix)
            .env("WINEDEBUG", "-all")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| format!("cannot run wineboot: {e}"))?;
        if !status.success() {
            logger::warn("wineboot exited non-zero, continuing anyway");
        }
    }

    let exe = version_dir.join(crate::packages::executable_name(true));
    if !exe.exists() {
        return Err(format!("Studio executable not found at {}", exe.display()));
    }

    logger::info(&format!("Starting Studio through Wine: {}", exe.display()));
    let mut command = std::process::Command::new(wine);
    command
        .arg(&exe)
        .env("WINEPREFIX", &prefix)
        .env("WINEDEBUG", "-all")
        .current_dir(version_dir);
    if !launch_args.is_empty() {
        command.arg(launch_args);
    }
    command
        .spawn()
        .map(Some)
        .map_err(|e| format!("Failed to start Roblox Studio: {e}"))
}

fn which_wine() -> Option<PathBuf> {
    ["wine", "wine64"].iter().find_map(|name| {
        std::process::Command::new("sh")
            .args(["-c", &format!("command -v {name} 2>/dev/null")])
            .output()
            .ok()
            .filter(|out| out.status.success() && !out.stdout.is_empty())
            .map(|out| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string()))
    })
}

/// Applies theme + background-app settings to Roblox's client storage.
pub fn apply_app_storage(paths: &Paths, settings: &Settings) {
    crate::appstorage::apply(paths, settings);
}

/// Sanity-checks the config files Ruststrap writes into the version directory
/// before spawning the client: a malformed ClientAppSettings.json makes the
/// client exit instantly, so surface the exact problem in the log.
fn verify_version_configs(version_dir: &Path) {
    let fflags = version_dir
        .join("ClientSettings")
        .join("ClientAppSettings.json");
    if fflags.exists() {
        match std::fs::read_to_string(&fflags) {
            Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(serde_json::Value::Object(map)) => {
                    logger::info(&format!(
                        "ClientAppSettings.json is valid ({} flags)",
                        map.len()
                    ));
                }
                Ok(_) => logger::warn("ClientAppSettings.json is empty or not a JSON object"),
                Err(e) => logger::error(&format!("ClientAppSettings.json is invalid: {e}")),
            },
            Err(e) => logger::error(&format!("Cannot read ClientAppSettings.json: {e}")),
        }
    }
}

/// Logs the size of the client's core binaries in the version directory so a
/// truncated/zero-byte extraction is caught before blaming anything else.
fn verify_core_files(version_dir: &Path, is_studio: bool) {
    let mut core = vec![crate::packages::executable_name(is_studio).to_string()];
    if cfg!(windows) && !is_studio {
        core.push("RobloxPlayerBeta.dll".to_string());
        core.push("WebView2Loader.dll".to_string());
    }
    for name in core {
        let path = version_dir.join(&name);
        let meta = std::fs::metadata(&path);
        match meta {
            Ok(meta) if meta.is_file() && meta.len() > 0 => {
                logger::info(&format!("{}: {} bytes", name, meta.len()));
            }
            Ok(_) => logger::error(&format!("{name} is empty - the install is broken; force a reinstall from the Versions tile")),
            Err(_) => logger::error(&format!("{name} is missing from {} - the install is incomplete; force a reinstall from the Versions tile", version_dir.display())),
        }
    }
}

/// Windows termination codes decoded into plain log text; the raw code is
/// always printed too.
fn describe_exit_code(code: Option<i32>) -> String {
    const NOT_FOUND: u32 = 0xC000_0135;
    const DLL_INIT: u32 = 0xC000_0142;
    const BAD_FORMAT: u32 = 0xC000_007B;
    const ACCESS_VIOLATION: u32 = 0xC000_0005;
    const FAIL_FAST: u32 = 0xC000_0409;
    const BREAKPOINT: u32 = 0x8000_0003;
    const ACCESS_DENIED: u32 = 0xC000_0022;
    let Some(code) = code else {
        return "terminated by a signal".to_string();
    };
    let raw = code as u32;
    let known: &[(u32, &str)] = &[
        (
            NOT_FOUND,
            "STATUS_DLL_NOT_FOUND - a required DLL is missing (broken install or antivirus)",
        ),
        (DLL_INIT, "STATUS_DLL_INIT_FAILED - a DLL failed to initialize"),
        (
            BAD_FORMAT,
            "STATUS_INVALID_IMAGE_FORMAT - corrupt or mixed-bitness binary",
        ),
        (ACCESS_VIOLATION, "STATUS_ACCESS_VIOLATION - the client crashed"),
        (
            FAIL_FAST,
            "STATUS_STACK_BUFFER_OVERRUN - typically the Hyperion anti-tamper terminating the client",
        ),
        (
            BREAKPOINT,
            "STATUS_BREAKPOINT - a debugger or anti-cheat intervened",
        ),
        (
            ACCESS_DENIED,
            "STATUS_ACCESS_DENIED - a security product may be blocking the executable",
        ),
    ];
    if raw == 0 {
        return "exit code 0 (clean exit; a running single-instance Roblox may have taken over)"
            .to_string();
    }
    let Some((_, text)) = known.iter().find(|(value, _)| *value == raw) else {
        return format!("exit code {raw} (0x{raw:08X})");
    };
    format!("exit code {raw} (0x{raw:08X}) - {text}")
}

/// A Roblox-written game log, matched the way Froststrap matches them: name
/// token `_Player_`/`_Studio_`, any extension, crash- and installer- dumps
/// excluded.
fn is_client_log(file_name: &str, is_studio: bool) -> bool {
    let token = if is_studio { "_Studio_" } else { "_Player_" };
    file_name.contains(token)
        && !file_name.contains("CrashHandler")
        && !file_name.contains("Installer")
}

/// Prints the tail of the newest Roblox-created log under
/// `<Roblox data dir>/Logs` so the client's own crash reason ends up in the
/// Ruststrap log the user can see. Falls back to the newest file of any name
/// written during the launch in case Roblox changes its naming again.
fn harvest_client_logs(paths: &Paths, is_studio: bool, after: SystemTime) {
    let dir = paths.roblox.join("Logs");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(_) => {
            logger::error(&format!(
                "There is no Roblox log directory at {} - the client died before Roblox created its log storage (any FastFlag content is then still unproven; check the exit code above)",
                dir.display()
            ));
            return;
        }
    };
    const SLOP: Duration = Duration::from_secs(2);
    let mut newest: Option<(SystemTime, PathBuf, bool)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let ok = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .filter(|mtime| *mtime + SLOP >= after);
        let Some(mtime) = ok else {
            continue;
        };
        let matched = is_client_log(name, is_studio);
        let better = match &newest {
            None => true,
            Some((best_time, _, best_matched)) => match (matched, *best_matched) {
                (true, false) => true,
                (false, true) => false,
                _ => mtime > *best_time,
            },
        };
        if better {
            newest = Some((mtime, path, matched));
        }
    }
    let (_, path, matched) = match newest {
        Some(newest) => newest,
        None => {
            logger::error(
                "Roblox wrote no log file during this launch - the client exited before initializing its logging; report the exit code above",
            );
            return;
        }
    };
    if !matched {
        logger::warn(
            "No _Player_-named log was found; dumping the newest file written during the launch",
        );
    }
    logger::error(&format!("Roblox client log: {}", path.display()));
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) => {
            logger::error(&format!("Cannot read Roblox client log: {e}"));
            return;
        }
    };
    const TAIL_LINES: usize = 50;
    let tail: Vec<&str> = text.lines().rev().take(TAIL_LINES).collect();
    logger::error(&format!("--- last {} lines ---", tail.len()));
    for line in tail.iter().rev() {
        logger::error(&format!("[Roblox] {line}"));
    }
    logger::error("--- end of Roblox client log ---");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_log_naming_matches_froststrap_tokens() {
        let player = "0.748.0.7481526_20261122T113100Z_Player_9aaf3_last.log";
        let studio = "0.748.0.7481526_20261122T113100Z_Studio_9aaf3_last.log";
        assert!(is_client_log(player, false));
        assert!(!is_client_log(player, true));
        assert!(is_client_log(studio, true));
        assert!(!is_client_log(
            "20261122T113100Z_CrashHandler_Player_x.log",
            false
        ));
        assert!(!is_client_log("RobloxInstaller_Player.log", false));
    }

    #[test]
    fn exit_code_decoding_names_known_statuses() {
        assert_eq!(
            describe_exit_code(Some(0)),
            "exit code 0 (clean exit; a running single-instance Roblox may have taken over)"
        );
        assert!(describe_exit_code(Some(-1073741515)).contains("STATUS_DLL_NOT_FOUND"));
        assert!(describe_exit_code(Some(-1073740791)).contains("Hyperion"));
        assert!(describe_exit_code(Some(42)).contains("42"));
        assert!(describe_exit_code(None).contains("signal"));
    }
}
