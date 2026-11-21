# Ruststrap

A native, headless Rust rewrite of [Froststrap](https://github.com/Froststrap/Froststrap) — a bootstrapper that installs, updates, patches, and launches Roblox. Licensed under MPL-2.0 to stay compatible with Froststrap.

## Principles

- **Headless and lightweight**: no UI, no tray, no updater daemon. One small binary, minimal dependencies, a lock file and a few JSON state files.
- **One theme, no configuration surface**: behavior matches Froststrap's defaults; no theming or profile options.
- **Targets only x64 Windows and x64 Linux**; nothing else is built or maintained.

## Platform split (mirroring Froststrap)

| | Windows | Linux |
|---|---|---|
| Player | `WindowsPlayer` packages, launched natively (`RobloxPlayerBeta.exe`) | [Sober](https://vinegarhq.org) via Flatpak — no packages downloaded |
| Studio | `WindowsStudio64` packages, launched natively (`RobloxStudioBeta.exe`) | `WindowsStudio64` packages run through **Wine** (system install, dedicated prefix in the data root) |

- **Windows**: registers the `roblox-player:` / `roblox:` / `roblox-studio:` / `roblox-studio-auth:` protocol handlers and `.rbxl*`/`.rbxm*` file associations under `HKCU`, tracks client locations and channels in a `Ruststrap` registry key, and can re-use verified packages already stocked by the official launcher's `%LOCALAPPDATA%/Roblox/Downloads` cache (same MD5 "FastHash" scheme).
- **Linux Player**: ensures Sober is installed from Flathub, maps `~/.var/app/org.vinegarhq.Sober` to Ruststrap's data root, translates `roblox-player:` URIs to `roblox://experiences/start?...` and syncs fast flags (with type coercion) into Sober's `config.json`.
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
  logger.rs        stderr + rolling log file
```

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
