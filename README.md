# Ruststrap

A native, headless Rust rewrite of [Froststrap](https://github.com/Froststrap/Froststrap) — a bootstrapper that installs, updates, patches, and launches Roblox. Licensed under MPL-2.0 to stay compatible with Froststrap.

## Principles

- **Lightweight**: one small binary, a lock file and a few JSON state files. No tray, no updater daemon.
- **Headless or UI**: protocol-handler and CLI invocations run fully headless; launching with no arguments (or `--ui`) opens a small GUI window.
- **One theme, no configuration surface**: behavior matches Froststrap's defaults; no theming or profile options.
- **Targets only x64 Windows and x64 Linux**; nothing else is built or maintained.

## UI

The window (`src/ui.rs`, egui) carries Froststrap's core feature set:

Every page scrolls if the window is too small, and all buttons share one uniform size.

- **Home**: Launch Player / Launch Studio buttons, optional reinstall-on-launch, an optional client version pin, installed vs. latest channel versions, Sober install/update status on Linux, and a live log pane; a progress bar with package counters while downloads run.
- **Fast Flags**: add, edit, and remove flags — stored in `ClientAppSettings.json` on Windows and written directly into Sober's `config.json` (typed values) on Linux; a "Reload" button re-reads the file (discarding unsaved edits).
- **Settings**: per-mode release channels, parallel download count, cleaner age, update/background toggles, Sober's own config options on Linux with an "Open file location" button (opens the folder in your file manager), "Clean now", folder shortcuts (data / Roblox / modifications / logs), protocol registration toggles (Windows), and a confirmed "Uninstall Roblox".
- **About**: version, links, license.

Background work runs on worker threads and reports through a shared progress slot and the log ring buffer, so the UI always stays responsive.

## Headless vs GUI

```sh
ruststrap                  # opens the UI
ruststrap --ui             # opens the UI
ruststrap roblox-player:.. # headless protocol flow
ruststrap --player --force # headless reinstall + launch
```

## Platform split (mirroring Froststrap)

| | Windows | Linux |
|---|---|---|
| Player | `WindowsPlayer` packages, launched natively (`RobloxPlayerBeta.exe`) | [Sober](https://vinegarhq.org) via Flatpak — no packages downloaded |
| Studio | `WindowsStudio64` packages, launched natively (`RobloxStudioBeta.exe`) | `WindowsStudio64` packages run through **Wine** (system install, dedicated prefix in the data root) |

- **Windows**: registers the `roblox-player:` / `roblox:` / `roblox-studio:` / `roblox-studio-auth:` protocol handlers and `.rbxl*`/`.rbxm*` file associations under `HKCU`, tracks client locations and channels in a `Ruststrap` registry key, and can re-use verified packages already stocked by the official launcher's `%LOCALAPPDATA%/Roblox/Downloads` cache (same MD5 "FastHash" scheme).
- **Linux Player**: ensures Sober is installed from Flathub, maps `~/.var/app/org.vinegarhq.Sober` to Ruststrap's data root, translates `roblox-player:` URIs to `roblox://experiences/start?...`. Fast flags and Sober's own options (GameMode, HiDPI, Discord RPC, …) are read from and written to Sober's `config.json` with typed values.
- **Linux Studio**: downloads the same Windows Studio packages, initializes a Wine prefix on first run, and launches `RobloxStudioBeta.exe` through it.
- **Channels**: per-mode channel support, arriving via `--channel`, a `channel:` token in the launch URI, or saved settings; private/unusable channels revert to `production` automatically (HTTP 401/403/404 detection on both the CDN and non-CDN settings endpoints).

## Layout

```
src/
  main.rs          CLI parsing (--player/-p, --studio/-s, --force, --no-launch, --no-wait,
                   --roblox-version, --game-shortcut, --unregister, bare URIs/files)
  bootstrapper.rs  orchestration and launch flows
  deployment.rs    mirror probing + channel handling
  manifest.rs      package manifest parsing / zip extraction
  packages.rs      download (parallel workers), extraction, modifications, cleanup
  gamejoin.rs      launch URI parsing / Sober URI translation
  fastflags.rs     ClientAppSettings.json management
  registry.rs      Windows-only HKCU registration
  sober.rs         Linux Player (Sober) flow
  launch.rs        process launching (native / Wine / Sober)
  appstorage.rs    Roblox app-storage settings (single theme)
  http.rs          HTTP + streaming MD5 verification
  state.rs         Settings.json / State.json / per-mode distribution state
  paths.rs         data/config layout per OS
  lock.rs          stale-tolerant single-instance lock
  watcher.rs       child-process wait
  cleaner.rs       log/cache cleanup
  progress.rs      shared progress slot for the UI
  logger.rs        stderr + rolling log file
  ui.rs            egui window (Home / Fast Flags / Settings / About)
```

Dependencies beyond the CLI set: `eframe`/`egui` (glow renderer, no wgpu) for the window.

## Building

```sh
cargo build --release          # Linux
cargo build --release --target x86_64-pc-windows-msvc   # Windows
```

Requirements at runtime: Flatpak (Linux Player for Sober), Wine (Linux Studio).

## CLI examples

```sh
ruststrap roblox-player:1+launchtime:0+placelauncherurl:https%3A%2F%2Fwww.roblox.com%2F...
ruststrap --game-shortcut 1234          # plain place join
ruststrap --game-shortcut "1234;jobId"  # 1:1 join A / private server
ruststrap --game-shortcut "1234;;code"  # 1:1 join A with access code
ruststrap --studio --channel alpha --no-launch
```

An empty `ruststrap roblox-player:` launches the installed player without a join.
