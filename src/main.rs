// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

//! Ruststrap: a native Rust rewrite of Froststrap, a cross-platform Roblox bootstrapper.
//! Headless by design: installs, updates, configures, and launches Roblox, then
//! optionally sticks around for cleanup while the game runs.

mod appstorage;
mod bootstrapper;
mod cleaner;
mod deployment;
mod fastflags;
mod gamejoin;
mod http;
mod launch;
mod lock;
mod logger;
mod manifest;
mod packages;
mod paths;
mod processes;
mod registry;
mod sober;
mod state;
mod watcher;

use bootstrapper::{Bootstrapper, LaunchMode};
use logger::info;

struct Args {
    mode: LaunchMode,
    payload: Option<String>,
    channel: Option<String>,
    version: Option<String>,
    force: bool,
    no_launch: bool,
    no_wait: bool,
    unregister: bool,
}

impl Default for Args {
    fn default() -> Self {
        Args {
            mode: LaunchMode::None,
            payload: None,
            channel: None,
            version: None,
            force: false,
            no_launch: false,
            no_wait: false,
            unregister: false,
        }
    }
}

const STUDIO_FILE_EXTENSIONS: [&str; 4] = [".rbxl", ".rbxlx", ".rbxm", ".rbxmx"];

const USAGE_EXIT_CODE: i32 = 2;

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut parsed = Args::default();

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let (flag, inline_value) = match arg.split_once('=') {
            Some((f, v)) => (f, Some(v.to_string())),
            None => (arg, None),
        };
        let mut inline_value = inline_value;
        i += 1;

        match flag {
            "--help" | "-h" => {
                eprintln!("{}", help_text());
                std::process::exit(0);
            }
            "--version" | "-V" => {
                println!("ruststrap {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--player" | "-p" => parsed.mode = LaunchMode::Player,
            "--studio" | "-s" => parsed.mode = LaunchMode::Studio,
            "--channel" => {
                let value = take_value(&mut inline_value, args, &mut i, "--channel")?;
                if value.is_empty() {
                    return Err("--channel requires a channel name".to_string());
                }
                parsed.channel = Some(value.to_ascii_lowercase());
            }
            "--roblox-version" => {
                parsed.version = Some(take_value(&mut inline_value, args, &mut i, "--roblox-version")?);
            }
            "--force" => parsed.force = true,
            "--no-launch" => parsed.no_launch = true,
            "--no-wait" => parsed.no_wait = true,
            "--no-gpu" | "--console" | "-c" | "-g" => {
                // Froststrap UI flags, accepted (and ignored) for compatibility
            }
            "--game-shortcut" => {
                let data = take_value(&mut inline_value, args, &mut i, "--game-shortcut")?;
                parsed.payload = gamejoin::game_shortcut_to_deeplink(&data);
                parsed.mode = LaunchMode::Player;
            }
            "--unregister" => parsed.unregister = true,
            other => {
                let target = other.to_string();
                let lower = target.to_ascii_lowercase();
                if lower.starts_with("roblox-player:") || lower.starts_with("roblox:") {
                    parsed.mode = LaunchMode::Player;
                    parsed.payload = Some(target);
                } else if lower.starts_with("roblox-studio-auth:") || lower.starts_with("roblox-studio:") {
                    parsed.mode = LaunchMode::Studio;
                    parsed.payload = Some(target);
                } else if target.starts_with("version-") {
                    parsed.version = Some(target);
                } else if STUDIO_FILE_EXTENSIONS.iter().any(|ext| lower.ends_with(ext)) {
                    parsed.mode = LaunchMode::Studio;
                    parsed.payload = Some(format!("-task EditFile -localPlaceFile \"{target}\""));
                } else {
                    return Err(format!("Unrecognized argument: {target}"));
                }
            }
        }
    }

    // A bare roblox URI works without an explicit mode; a version pin with no
    // mode defaults to the player; everything else defaults to a plain player launch.
    if parsed.mode == LaunchMode::None {
        if let Some(payload) = &parsed.payload {
            if payload.starts_with("roblox") {
                parsed.mode = LaunchMode::Player;
            } else {
                return Err(format!("Unrecognized target: {payload}"));
            }
        } else {
            parsed.mode = LaunchMode::Player;
        }
    }

    Ok(parsed)
}

fn take_value(inline: &mut Option<String>, args: &[String], i: &mut usize, flag: &str) -> Result<String, String> {
    if let Some(value) = inline.take() {
        return Ok(value);
    }
    match args.get(*i) {
        Some(value) => {
            *i += 1;
            Ok(value.clone())
        }
        None => Err(format!("{flag} requires a value")),
    }
}

fn help_text() -> String {
    format!(
        "\
Ruststrap v{} - a native Rust rewrite of Froststrap (a cross-platform Roblox bootstrapper)

Usage:
  ruststrap [OPTIONS] [TARGET]

Targets:
  roblox-player:<uri>            Launch the Roblox Player with the given launch URI
  roblox-studio:<uri>            Launch Roblox Studio with the given launch URI
  <place file>.rbxl[x]|.rbxm[x]  Edit the given place file with Roblox Studio
  version-<hash>                 Pin the Roblox client version

Options:
  -p, --player                   Launch the Roblox Player (default)
  -s, --studio                   Launch Roblox Studio
      --channel <name>           Use the given Roblox release channel
      --roblox-version <hash>    Pin the Roblox client version
      --game-shortcut <data>     Launch a game from shortcut data (placeId[;jobId[;accessCode]])
      --force                    Force a reinstallation of Roblox
      --no-launch                Install or update Roblox without launching it
      --no-wait                  Do not stay alive while Roblox is running
      --unregister               Remove Roblox protocol handlers (Windows)
  -h, --help                     Show this help
  -V, --version                  Show version

Configuration: edit Settings.json, ClientAppSettings.json under the
Ruststrap config directory (see README.md).",
        env!("CARGO_PKG_VERSION")
    )
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let parsed = match parse_args(&args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            info(&format!("Exiting: {message}"));
            std::process::exit(USAGE_EXIT_CODE);
        }
    };

    let paths = paths::init();
    let log_path = paths.logs.join("Ruststrap.log");
    logger::init(log_path);
    info(&format!("Log file: {}", paths.logs.join("Ruststrap.log").display()));

    let ctx = state::Ctx::load(paths);

    if parsed.unregister {
        #[cfg(windows)]
        registry::unregister_all();
        info("Protocol registration removed");
        return;
    }

    let payload = parsed.payload.unwrap_or_default();
    info(&format!("Launch args: {payload}"));

    let mut ctx_boxed = ctx;
    let http = http::Http::new();
    let mut bootstrapper = Bootstrapper::new(
        &http,
        &mut ctx_boxed,
        parsed.mode,
        payload,
        parsed.version,
        parsed.force,
        parsed.no_launch,
        parsed.no_wait,
    )
    .with_channel_override(parsed.channel);

    if let Err(error) = bootstrapper.run() {
        logger::error(&format!("Bootstrapper failed: {error}"));
        eprintln!("Error: {error}");
        std::process::exit(1);
    }
}
