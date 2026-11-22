# Froststrap → Ruststrap feature parity

Ruststrap deliberately targets a single lightweight binary on x64 Windows and
x64 universal Linux, with one fixed UI theme and no configuration surface for
the bootstrapper itself. Features below are tracked in three groups: already
covered, deliberately rejected, and planned.

## Already covered in Ruststrap

| Froststrap feature | Ruststrap equivalent |
|---|---|
| Switch Roblox update channels easily | Settings → Channels, per client type |
| Automatic / prompt / blocked channel changes | `Channel change mode` |
| Fixed folder instead of `version-xxxxx` | `Static directory` setting |
| Block Roblox updates entirely | `Update Roblox` toggle; on Windows the stock client updates through winget |
| FastFlag editor (add / import / delete / export / search / inline edit / rename) | Fast Flags page |
| Game shortcuts for quick joins | `--game-shortcut` and the headless URI flows |
| Log / cache cleaner | Settings → Clean now + cleaner age |
| Uninstall / reinstall clients | Settings → Uninstall, Reinstall toggle |
| Roblox user settings editor (framerate, volumes, sensitivity, …) | Roblox Settings page |
| Disable screenshot & video recording | Best-effort preset toggle in the Fast Flags page (`FFlagEnableCapturesHotkeyExperiment_v4`; not on Roblox's current allowlist) |
| Automatic rejoin after disconnect | `Auto-rejoin after a disconnect` setting; client logs are scanned for disconnect evidence after exit and the same place is rejoined (max 3 times, 3 s delay) |
| Import settings from Bloxstrap/Fishstrap | Settings → Import from another bootstrapper (Froststrap everywhere; Bloxstrap/Fishstrap on Windows), imports flags, settings, and saved flag profiles |
| FastFlag allowlist checks + "Clean List" | Non-allowlisted flags are marked in the editor, warned about when added, and removable via `Clean non-allowlisted` (September 2025 allowlist) |
| FastFlag profiles | Profile combo with Save / Load / Delete, compatible with Froststrap's `SavedFlagProfiles` files |
| View all available Roblox channels | Settings → Channels → `Show available channels` probes every known channel live (Roblox has no public channel-list endpoint) |

## Won't be implemented (by design)

- **Multi-instance launching** — Ruststrap runs the stock winget-managed client on Windows and Sober on Linux; neither setup participates in the single-instance mutex workaround this feature depends on.
- **Mods manager (community mods, multi-mod apply, hex-color generators, cursor sets, custom font/death-sound files)** — a remote mod catalog, per-client mod layering, generation UI, and caches go directly against the project's lightweight, minimal-by-design goals.
- **Theme engine (image/gradient backgrounds, custom fonts, Aero/Acrylic/Mica backdrops, fully customizable launcher)** — rejected by the same mentality: Ruststrap ships exactly one tuned theme to stay small and fast.

## Planned additions

- **Playtime counter (total + per session)** — session bookkeeping across launches with a persistent store.
- **Discord RPC (player, Studio, custom status)** — Discord IPC socket client with process-status awareness.
- **Account manager** — secure .ROBLOSECURITY/credential handling, switching, and storage.
- **macOS support (aarch64/x64 installers)** — a third build target with its own launch flow.
- **Native packaging (.msi/.deb/.rpm/AppImage/AUR)** — release-engineering work for per-platform installers beyond the single portable binary.

## Not implemented (cheapest → most expensive, candidates)

1. **Region selector / region-joined sessions** — needs Roblox region metadata, join-argument plumbing, and UI; medium-to-large with real correctness risk around server types.
2. **System tray integration (background operation, region join from tray)** — egui has no tray support; needs a tray crate, event-loop bridging, and platform menus — a real dependency and complexity jump for headless-lightweight goals.
