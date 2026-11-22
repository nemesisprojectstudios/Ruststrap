# Froststrap → Ruststrap feature parity

Ruststrap deliberately targets a single lightweight binary on x64 Windows and
x64 universal Linux, with one fixed UI theme and no configuration surface for
the bootstrapper itself. Features below are those Froststrap has that Ruststrap
does not, ordered from cheapest to most expensive to implement (effort, code
size, and runtime weight). Rationale: one line each on why they were not added.

## Already covered in Ruststrap

| Froststrap feature | Ruststrap equivalent |
|---|---|
| Switch Roblox update channels easily | Settings → Channels, per client type |
| Automatic / prompt / blocked channel changes | `Channel change mode` |
| Fixed folder instead of `version-xxxxx` | `Static directory` setting |
| Block Roblox updates entirely | `Update Roblox` toggle; on Windows the stock client updates through winget |
| FastFlag editor (add / import / delete / export / search / inline edit) | Fast Flags page |
| Game shortcuts for quick joins | `--game-shortcut` and the headless URI flows |
| Log / cache cleaner | Settings → Clean now + cleaner age |
| Uninstall / reinstall clients | Settings → Uninstall, Reinstall toggle |
| Roblox user settings editor (framerate, volumes, sensitivity, …) | Roblox Settings page |

## Not implemented (cheapest → most expensive)

1. **Disable screenshot & video recording** — cheapest: a two-flag FastFlag preset, but it needs no new UI, so it stayed a manual flag users can add themselves.
2. **Automatic rejoin after disconnect (inactivity)** — needs last-place/job tracking plus re-launch logic on abnormal client exit; small, but it silently resets gameplay state if it misfires.
3. **Multi-instance launching** — requires maintaining Roblox's single-instance mutex workaround, which interacts with the stock client that Ruststrap no longer controls on Windows.
4. **Import settings from Bloxstrap/Fishstrap** — needs parsers for two foreign config formats plus key mapping; small code but a compatibility surface that must track external format changes.
5. **FastFlag allowlist checks + "Clean List"** — requires fetching and caching Roblox's flag allowlist and surfacing per-flag warnings in the editor; medium UI and network work.
6. **FastFlag profiles** — multiple named flag sets with switching; medium editor/storage/UI work, deferrable because Sober's single `config.json` makes profile semantics murkier on Linux.
7. **View all available Roblox channels** — depends on an undocumented channel-list endpoint that can disappear; medium work with external fragility.
8. **Region selector / region-joined sessions** — needs Roblox region metadata, join-argument plumbing, and UI; medium-to-large with real correctness risk around server types.
9. **Playtime counter (total + per session)** — requires session bookkeeping across launches and a persistent store; medium, and it adds a privacy-charged dataset to the app.
10. **Discord RPC (player, Studio, custom status)** — means a Discord IPC socket client and process-status scraping; a meaningful dependency/code weight for a cosmetic feature.
11. **Mods manager (community mods, multi-mod apply, hex-color generators, cursor sets, custom font/death-sound files)** — the largest "feature" item: a remote mod catalog, per-client mod layering, generation UI, and caches; big code, UI, and network weight.
12. **System tray integration (background operation, region join from tray)** — egui has no tray support; needs a tray crate, event-loop bridging, and platform menus — a real dependency and complexity jump for headless-lightweight goals.
13. **Account manager** — implies credential/.ROBLOSECURITY handling, switching, and secure storage; the most security-sensitive and heaviest feature, out of proportion for a launch gate.
14. **Theme engine (image/gradient backgrounds, themes, custom fonts, Aero/Acrylic/Mica backdrops, fully customizable launcher)** — rejected by design: Ruststrap ships exactly one tuned theme to stay small and fast; an image/texture theme system raises RAM, size, and platform-specific compositing APIs.
15. **macOS support (aarch64/i64 installers)** — out of scope by the defined build targets (x64 Windows + x64 universal Linux only).
16. **Native packaging (.msi/.deb/.rpm/AppImage/AUR)** — release engineering rather than a code feature; adds substantial CI/packaging weight and per-distro maintenance for a single-binary distribution model.
