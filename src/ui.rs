// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

//! egui-based UI mirroring Froststrap's feature set, in one window and one theme:
//! launch/update with progress and a live log, a fast flag editor, channel and
//! cleaner settings, folder shortcuts, uninstall and (Windows) protocol toggles.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use eframe::egui;
use serde_json::Value;

use crate::bootstrapper::{Bootstrapper, LaunchMode};
use crate::fastflags;
use crate::gbs;
use crate::http::Http;
use crate::logger;
use crate::packages;
use crate::paths::{self, DEFAULT_CHANNEL};
use crate::progress;
use crate::sober;
use crate::state::Ctx;

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Home,
    FastFlags,
    Roblox,
    Settings,
    About,
}

enum WorkerMsg {
    Versions {
        player: Option<String>,
        studio: Option<String>,
    },
    SoberInstalled(bool),
    AppVersion(Option<String>),
    UserChannels {
        player: Option<String>,
        studio: Option<String>,
    },
    ChannelProbes(Vec<(&'static str, Option<String>)>),
    FilePicked(FilePickTarget, Option<std::path::PathBuf>),
}

#[derive(Clone, Copy, PartialEq)]
enum FilePickTarget {
    FlagsImport,
    RobloxImport,
    RobloxExport,
    BootstrapperRoot,
}

#[derive(Clone, Copy, PartialEq)]
enum AddTab {
    Single,
    Json,
}

/// Bootstrappers whose configuration Ruststrap can import.
#[derive(Clone, Copy, PartialEq)]
enum ImportSource {
    Froststrap,
    #[cfg(windows)]
    Bloxstrap,
    #[cfg(windows)]
    Fishstrap,
    Browse,
}

type SharedCtx = Arc<Mutex<Ctx>>;

/// Every button uses this size so the layout stays uniform.
const BUTTON_SIZE: [f32; 2] = [150.0, 32.0];
const ACTION_SIZE: [f32; 2] = [142.0, 32.0];

/// Uniform size of a settings tile and the gap between tiles.
const TILE_SIZE: [f32; 2] = [320.0, 190.0];
const TILE_GAP: f32 = 10.0;
const TILE_INNER_GAP: f32 = 4.0;

type TileBody<A> = Box<dyn Fn(&mut egui::Ui, &mut A)>;

pub fn run(ctx: Ctx) {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([820.0, 580.0])
            .with_min_inner_size([660.0, 460.0]),
        ..Default::default()
    };
    let result = eframe::run_native(
        "Ruststrap",
        options,
        Box::new(|cc| {
            cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
            cc.egui_ctx.all_styles_mut(|style| {
                style.spacing.button_padding = egui::vec2(10.0, 6.0);
                // egui's built-in scroll fade paints a translucent band over
                // scrolled tile content; tiles need clean clip edges instead.
                style.spacing.scroll.fade.strength = 0.0;
            });
            let mut app = RuststrapApp::new(ctx);
            app.refresh_profiles();
            Ok(Box::new(app))
        }),
    );
    if let Err(e) = result {
        logger::error(&format!("UI failed: {e}"));
        eprintln!("UI error: {e}");
        std::process::exit(1);
    }
}

struct RuststrapApp {
    shared: SharedCtx,
    tx: mpsc::Sender<WorkerMsg>,
    rx: mpsc::Receiver<WorkerMsg>,
    busy: Arc<AtomicBool>,
    page: Page,
    fflags: serde_json::Map<String, Value>,
    flag_edits: HashMap<String, String>,
    flag_selected: HashSet<String>,
    flag_search: String,
    flag_dirty: bool,
    confirm_delete_all: bool,
    confirm_armed_at: Option<std::time::Instant>,
    add_dialog_open: bool,
    export_dialog_open: bool,
    picking: Option<FilePickTarget>,
    dialog_tab: AddTab,
    dialog_error: Option<String>,
    dialog_name: String,
    dialog_value: String,
    import_text: String,
    export_text: String,
    gbs: gbs::Gbs,
    gbs_edits: GbsEdits,
    gbs_init: bool,
    gbs_exists: bool,
    gbs_dirty: bool,
    gbs_read_only: bool,
    gbs_save_error: Option<String>,
    settings_edits: SettingsEdits,
    settings_dirty: bool,
    confirm_uninstall: bool,
    force_reinstall: bool,
    version_pin: String,
    latest_player: Option<String>,
    latest_studio: Option<String>,
    user_channel_player: Option<String>,
    user_channel_studio: Option<String>,
    app_latest: Option<String>,
    renaming: Option<String>,
    rename_buf: String,
    sober_installed: bool,
    sober_toggles: Vec<(&'static str, &'static str, bool)>,
    flag_profiles: Vec<String>,
    profile_sel: String,
    profile_name: String,
    channel_status: Option<Vec<(&'static str, Option<String>)>>,
    import_dialog_open: bool,
    import_source: usize,
    import_summary: Option<String>,
}

/// Sober's own options (Froststrap's SoberSettingsManager preset keys).
const SOBER_TOGGLES: [(&str, &str); 10] = [
    ("Allow gamepad permission", "allow_gamepad_permission"),
    ("Enable GameMode", "enable_gamemode"),
    ("Enable HiDPI", "enable_hidpi"),
    ("Use console experience", "use_console_experience"),
    ("Use libsecret", "use_libsecret"),
    ("Use OpenGL", "use_opengl"),
    (
        "Server location indicator",
        "server_location_indicator_enabled",
    ),
    ("Discord RPC", "discord_rpc_enabled"),
    ("Discord RPC join button", "discord_rpc_show_join_button"),
    ("Close on leave", "close_on_leave"),
];

/// Roblox's public deployment channels (Froststrap's channel picker list).
/// Roblox has no public channel-list endpoint, so this fixed list is probed
/// live instead.
const KNOWN_CHANNELS: [&str; 6] = [
    "production",
    "zlive",
    "znext",
    "zintegration",
    "zcanary",
    "zflag",
];

#[derive(Default)]
struct SettingsEdits {
    player_channel: String,
    studio_channel: String,
    channel_change_mode: String,
    cleaner_days: u32,
    max_threads: usize,
    update_roblox: bool,
    background_app: bool,
    auto_rejoin: bool,
    roblox_domain: String,
    static_directory: bool,
    theme: String,
}

/// Typed view of the UserGameSettings values we expose, mirroring
/// Froststrap's GlobalSettingsViewModel (same keys, same defaults).
#[derive(Default)]
struct GbsEdits {
    framerate_cap: i64,
    quality_level: i64,
    fullscreen: bool,
    max_quality: bool,
    vignette: bool,
    master_volume: f64,
    studio_volume: f64,
    party_voice: f64,
    voice_chat: f64,
    mouse_sensitivity: f64,
    shift_lock: bool,
    fps_x: String,
    fps_y: String,
    tps_x: String,
    tps_y: String,
    camera_y_inverted: bool,
    haptic_strength: f64,
    ui_transparency: String,
    font_size: i64,
    player_list_layout: i64,
    reduced_motion: bool,
    perf_stats: bool,
    chat_translation: bool,
    vr: bool,
}

fn gbs_bool_row(ui: &mut egui::Ui, label: &str, value: &mut bool, dirty: &mut bool) {
    if ui.checkbox(value, label).changed() {
        *dirty = true;
    }
}

impl RuststrapApp {
    /// Re-reads GlobalBasicSettings from disk into the typed edit view.
    fn reload_gbs(&mut self) {
        self.gbs = gbs::Gbs::load(paths::get());
        self.gbs_exists = gbs::file_location(paths::get()).exists();
        self.gbs_read_only = gbs::Gbs::is_read_only();
        self.gbs_save_error = None;
        self.gbs_dirty = false;
        let g = &self.gbs;
        self.gbs_edits = GbsEdits {
            framerate_cap: g
                .get("int", "FramerateCap")
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(60),
            quality_level: g
                .get("token", "SavedQualityLevel")
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(1),
            fullscreen: g.get_bool("Fullscreen").unwrap_or(false),
            max_quality: g.get_bool("MaxQualityEnabled").unwrap_or(false),
            vignette: g
                .get_bool("VignetteEnabled")
                .or_else(|| g.get_bool("VignetteEnabledCustomOption"))
                .unwrap_or(false),
            master_volume: g.get_float("MasterVolume").unwrap_or(1.0),
            studio_volume: g.get_float("MasterVolumeStudio").unwrap_or(1.0),
            party_voice: g.get_float("PartyVoiceVolume").unwrap_or(1.0),
            voice_chat: g.get_float("VoiceChatVolume").unwrap_or(1.0),
            mouse_sensitivity: g.get_float("MouseSensitivity").unwrap_or(1.0),
            shift_lock: g
                .get("token", "ControlMode")
                .map(|v| v.trim() == "1")
                .unwrap_or(false),
            fps_x: g
                .get_vector("MouseSensitivityFirstPerson", "X")
                .unwrap_or_default(),
            fps_y: g
                .get_vector("MouseSensitivityFirstPerson", "Y")
                .unwrap_or_default(),
            tps_x: g
                .get_vector("MouseSensitivityThirdPerson", "X")
                .unwrap_or_default(),
            tps_y: g
                .get_vector("MouseSensitivityThirdPerson", "Y")
                .unwrap_or_default(),
            camera_y_inverted: g.get_bool("CameraYInverted").unwrap_or(false),
            haptic_strength: g.get_float("HapticStrength").unwrap_or(0.0),
            ui_transparency: g.get("float", "PreferredTransparency").unwrap_or_default(),
            font_size: g
                .get("token", "PreferredTextSize")
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(1),
            player_list_layout: g
                .get("token", "PeoplePageLayout")
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0),
            reduced_motion: g.get_bool("ReducedMotion").unwrap_or(false),
            perf_stats: g.get_bool("PerformanceStatsVisible").unwrap_or(false),
            chat_translation: g.get_bool("ChatTranslationEnabled").unwrap_or(false),
            vr: g.get_bool("VREnabled").unwrap_or(false),
        };
    }

    fn new(ctx: Ctx) -> RuststrapApp {
        let (tx, rx) = mpsc::channel();
        let fflags = Self::load_flags();
        let mut sober_toggles = Vec::new();
        if !cfg!(windows) {
            let config = sober::load_config(paths::get());
            for (label, key) in SOBER_TOGGLES {
                let on = sober::config_bool(&config, key).unwrap_or(false);
                sober_toggles.push((label, key, on));
            }
        }
        let settings_edits = SettingsEdits {
            player_channel: ctx.settings.player_channel.clone(),
            studio_channel: ctx.settings.studio_channel.clone(),
            channel_change_mode: ctx.settings.channel_change_mode.clone(),
            cleaner_days: ctx.settings.cleaner_days,
            max_threads: ctx.settings.max_thread_download,
            update_roblox: ctx.settings.update_roblox,
            background_app: ctx.settings.background_app,
            auto_rejoin: ctx.settings.auto_rejoin,
            roblox_domain: ctx.settings.roblox_domain.clone(),
            static_directory: ctx.settings.static_directory,
            theme: ctx.settings.theme.clone(),
        };
        RuststrapApp {
            shared: Arc::new(Mutex::new(ctx)),
            tx,
            rx,
            busy: Arc::new(AtomicBool::new(false)),
            page: Page::Home,
            fflags,
            flag_edits: HashMap::new(),
            flag_selected: HashSet::new(),
            flag_search: String::new(),
            flag_dirty: false,
            confirm_delete_all: false,
            confirm_armed_at: None,
            add_dialog_open: false,
            export_dialog_open: false,
            picking: None,
            dialog_tab: AddTab::Single,
            dialog_error: None,
            dialog_name: String::new(),
            dialog_value: String::new(),
            import_text: String::new(),
            export_text: String::new(),
            gbs: gbs::Gbs::default(),
            gbs_edits: GbsEdits::default(),
            gbs_init: false,
            gbs_exists: false,
            gbs_dirty: false,
            gbs_read_only: false,
            gbs_save_error: None,
            settings_edits,
            settings_dirty: false,
            confirm_uninstall: false,
            force_reinstall: false,
            version_pin: String::new(),
            latest_player: None,
            latest_studio: None,
            user_channel_player: None,
            user_channel_studio: None,
            app_latest: None,
            renaming: None,
            rename_buf: String::new(),
            sober_installed: !cfg!(windows) && sober::is_installed(),
            sober_toggles,
            flag_profiles: Vec::new(),
            profile_sel: String::new(),
            profile_name: String::new(),
            channel_status: None,
            import_dialog_open: false,
            import_source: 0,
            import_summary: None,
        }
    }

    // Flags live in ClientAppSettings.json on Windows; in Sober's
    // config.json on Linux (Sober never reads ClientAppSettings).
    fn load_flags() -> serde_json::Map<String, Value> {
        if cfg!(windows) {
            fastflags::load_or_init(paths::get())
        } else {
            sober::load_config(paths::get())
                .get("fflags")
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_default()
        }
    }

    fn reload_flags(&mut self) {
        self.fflags = Self::load_flags();
        self.flag_edits.clear();
        self.flag_selected.clear();
        self.flag_dirty = false;
        let (source, file) = if cfg!(windows) {
            ("file", fastflags::fflags_path(paths::get()))
        } else {
            ("Sober config", sober::config_file(paths::get()))
        };
        logger::info(&format!(
            "Fast flags loaded: {} entries from {} ({})",
            self.fflags.len(),
            source,
            file.display()
        ));
    }

    fn open_file_dialog(
        &mut self,
        target: FilePickTarget,
        title: &'static str,
        filter_name: &'static str,
        extensions: &'static [&'static str],
        save_as: Option<&'static str>,
    ) {
        if self.picking.is_some() {
            return;
        }
        self.picking = Some(target);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            // rfd blocks until the user picks a file; run it off the UI
            // thread so the compositor doesn't see a stalled app (ANR).
            let picked = std::panic::catch_unwind(|| {
                let dialog = rfd::FileDialog::new()
                    .set_title(title)
                    .add_filter(filter_name, extensions);
                if let Some(name) = save_as {
                    dialog.set_file_name(name).save_file()
                } else {
                    dialog.pick_file()
                }
            });
            let picked = picked.unwrap_or_else(|_| {
                logger::error("File dialog crashed");
                None
            });
            let _ = tx.send(WorkerMsg::FilePicked(target, picked));
        });
    }

    /// Directory-picker variant of the off-thread rfd flow.
    fn open_dir_dialog(&mut self, target: FilePickTarget, title: &'static str) {
        if self.picking.is_some() {
            return;
        }
        self.picking = Some(target);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let picked =
                std::panic::catch_unwind(|| rfd::FileDialog::new().set_title(title).pick_folder());
            let picked = picked.unwrap_or_else(|_| {
                logger::error("Folder dialog crashed");
                None
            });
            let _ = tx.send(WorkerMsg::FilePicked(target, picked));
        });
    }

    fn handle_file_pick(&mut self, target: FilePickTarget, file: Option<std::path::PathBuf>) {
        let Some(file) = file else {
            logger::info("File dialog cancelled");
            return;
        };
        match target {
            FilePickTarget::FlagsImport => {
                let parsed = std::fs::read_to_string(&file)
                    .map_err(|e| e.to_string())
                    .and_then(|text| {
                        parse_flags_json(&text)
                            .ok_or_else(|| "not a JSON object of flag names to values".to_string())
                    });
                match parsed {
                    Ok((map, count)) => {
                        self.fflags.extend(map);
                        self.flag_edits.clear();
                        self.flag_dirty = true;
                        self.dialog_error = None;
                        self.add_dialog_open = false;
                        self.import_text.clear();
                        logger::info(&format!("Imported {count} flags from {}", file.display()));
                    }
                    Err(e) => self.dialog_error = Some(format!("Import failed: {e}")),
                }
            }
            FilePickTarget::RobloxImport => match gbs::Gbs::import(&file) {
                Ok(()) => self.reload_gbs(),
                Err(e) => logger::error(&format!("Failed to import Roblox settings: {e}")),
            },
            FilePickTarget::BootstrapperRoot => {
                self.import_dialog_open = true;
                self.import_summary = Some(match self.import_from_bootstrapper(&file) {
                    Ok(summary) => summary,
                    Err(e) => format!("Import failed: {e}"),
                });
            }
            FilePickTarget::RobloxExport => {
                let doc = gbs::Gbs {
                    raw: self.gbs.raw.clone(),
                };
                if let Err(e) = doc.export(&file) {
                    logger::error(&format!("Failed to export Roblox settings: {e}"));
                }
            }
        }
    }

    fn spawn_job(
        &mut self,
        title: &'static str,
        work: impl FnOnce(&SharedCtx, &mpsc::Sender<WorkerMsg>) + Send + 'static,
    ) {
        if self.busy.swap(true, Ordering::SeqCst) {
            logger::warn("Another operation is already running");
            return;
        }
        let tx = self.tx.clone();
        let shared = self.shared.clone();
        let busy = self.busy.clone();
        std::thread::spawn(move || {
            progress::set(title);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                work(&shared, &tx);
            }));
            if result.is_err() {
                logger::error(&format!("{title} crashed (panic)"));
            }
            progress::clear();
            busy.store(false, Ordering::SeqCst);
        });
    }

    fn launch_job(&mut self, mode: LaunchMode, force: bool, pin: Option<String>) {
        self.spawn_job("Launching Roblox", move |shared, _| {
            let outcome = match shared.lock() {
                Ok(mut guard) => {
                    let http = Http::new();
                    let mut bootstrapper = Bootstrapper::new(
                        &http,
                        &mut guard,
                        mode,
                        String::new(),
                        pin,
                        force,
                        false,
                        true,
                    );
                    bootstrapper.run()
                }
                Err(e) => Err(format!("state lock unavailable: {e}")),
            };
            match outcome {
                Ok(()) => logger::info("Launch finished"),
                Err(e) => logger::error(&format!("Launch failed: {e}")),
            }
        });
    }

    fn pinned_version(&self) -> Option<String> {
        let pin = self.version_pin.trim();
        if pin.is_empty() {
            None
        } else {
            Some(pin.to_string())
        }
    }

    /// Installs Sober when missing (update=false) or updates it (update=true).
    fn sober_job(&mut self, update: bool) {
        let title = if update {
            "Updating Sober"
        } else {
            "Installing Sober"
        };
        self.spawn_job(title, move |_, tx| {
            let outcome = if update {
                sober::update()
            } else {
                sober::ensure_installed()
            };
            match outcome {
                Ok(()) => {}
                Err(e) => logger::error(&format!("{title} failed: {e}")),
            }
            let _ = tx.send(WorkerMsg::SoberInstalled(sober::is_installed()));
        });
    }

    /// Compares this build's version against the latest GitHub release tag.
    fn check_app_version_job(&mut self) {
        self.spawn_job("Checking Ruststrap version", move |_, tx| {
            let http = Http::new();
            let latest = http
                .get_json_checked::<serde_json::Value>(
                    "https://api.github.com/repos/nemesisprojectstudios/Ruststrap/releases/latest",
                )
                .ok()
                .and_then(|v| {
                    v.get("tag_name")
                        .and_then(|t| t.as_str())
                        .map(|t| t.trim_start_matches('v').to_string())
                });
            if latest.is_none() {
                logger::warn("Could not fetch the latest Ruststrap release");
            }
            let _ = tx.send(WorkerMsg::AppVersion(latest));
        });
    }

    fn check_versions_job(&mut self) {
        self.spawn_job("Checking versions", move |shared, tx| {
            let http = Http::new();
            let guard = match shared.lock() {
                Ok(g) => g,
                Err(e) => {
                    logger::error(&format!("state lock unavailable: {e}"));
                    return;
                }
            };
            let decode = |is_studio: bool| {
                let channel = if is_studio {
                    &guard.settings.studio_channel
                } else {
                    &guard.settings.player_channel
                };
                match crate::deployment::get_client_version(
                    &http,
                    &guard.settings,
                    channel,
                    crate::packages::binary_type(is_studio),
                ) {
                    Ok(info) => Some(info.version),
                    Err(e) => {
                        logger::warn(&format!("Version check failed: {e}"));
                        None
                    }
                }
            };
            // Studio is packaged on every platform; on Linux the player is Sober's.
            let player = if cfg!(windows) { decode(false) } else { None };
            let studio = decode(true);
            let _ = tx.send(WorkerMsg::Versions { player, studio });
        });
    }

    /// Asks Roblox which channel the user account is enrolled in per binary
    /// type; on Linux the player is Sober's, so only Studio is probed there.
    fn check_user_channels_job(&mut self) {
        self.spawn_job("Checking user channels", move |shared, tx| {
            let http = Http::new();
            let guard = match shared.lock() {
                Ok(g) => g,
                Err(e) => {
                    logger::error(&format!("state lock unavailable: {e}"));
                    return;
                }
            };
            let decode = |is_studio: bool| {
                // the player is Sober's mobile client on Linux, whose account
                // channel is reported under its own binary type
                let binary = if is_studio {
                    crate::packages::binary_type(true)
                } else if cfg!(windows) {
                    crate::packages::binary_type(false)
                } else {
                    crate::sober::ROBLOX_BINARY_TYPE
                };
                crate::deployment::get_user_channel(&http, &guard.settings, binary)
            };
            let player = decode(false);
            let studio = decode(true);
            let _ = tx.send(WorkerMsg::UserChannels { player, studio });
        });
    }

    /// Probes every known channel for live deployment info. Roblox has no
    /// public channel-list endpoint, so availability = a version answers.
    /// The mobile endpoints reject unauthenticated requests for every
    /// channel, so availability is probed via the public Windows deploy
    /// info; Sober's own channel comes from the user-channel job.
    fn probe_channels_job(&mut self) {
        self.spawn_job("Probing channels", move |shared, tx| {
            let http = Http::new();
            let domain = shared
                .lock()
                .ok()
                .map(|guard| guard.settings.roblox_domain.clone())
                .filter(|domain| !domain.trim().is_empty())
                .unwrap_or_else(|| crate::paths::ROBLOX_DOMAIN.to_string());
            let settings = crate::state::Settings {
                roblox_domain: domain,
                ..Default::default()
            };
            let mut probes = Vec::new();
            for channel in KNOWN_CHANNELS {
                match crate::deployment::get_client_version(
                    &http,
                    &settings,
                    channel,
                    crate::deployment::CHANNEL_PROBE_BINARY,
                ) {
                    Ok(info) => probes.push((channel, Some(info.version))),
                    Err(_) => probes.push((channel, None)),
                }
            }
            let _ = tx.send(WorkerMsg::ChannelProbes(probes));
        });
    }

    fn refresh_profiles(&mut self) {
        self.flag_profiles = fastflags::list_profiles(paths::get());
        if !self.profile_sel.is_empty() && !self.flag_profiles.contains(&self.profile_sel.clone())
        {
            self.profile_sel.clear();
        }
    }
}

impl eframe::App for RuststrapApp {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // workers only repaint on messages; poll while one is running so the
        // log feed and progress bar keep moving without user input
        if self.busy.load(Ordering::SeqCst) {
            root.ctx()
                .request_repaint_after(std::time::Duration::from_millis(300));
        }
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                WorkerMsg::Versions { player, studio } => {
                    self.latest_player = player;
                    self.latest_studio = studio;
                }
                WorkerMsg::SoberInstalled(installed) => self.sober_installed = installed,
                WorkerMsg::AppVersion(latest) => self.app_latest = latest,
                WorkerMsg::UserChannels { player, studio } => {
                    self.user_channel_player = player;
                    self.user_channel_studio = studio;
                }
                WorkerMsg::ChannelProbes(probes) => self.channel_status = Some(probes),
                WorkerMsg::FilePicked(target, file) => {
                    self.picking = None;
                    self.handle_file_pick(target, file);
                }
            }
        }

        egui::Panel::left("nav").resizable(false).show(root, |ui| {
            ui.add_space(12.0);
            ui.heading("Ruststrap");
            ui.add_space(12.0);
            for (page, label) in [
                (Page::Home, "Home"),
                (Page::FastFlags, "Fast Flags"),
                (Page::Roblox, "Roblox settings"),
                (Page::Settings, "Settings"),
                (Page::About, "About"),
            ] {
                let fill = if self.page == page {
                    ui.visuals().selection.bg_fill
                } else {
                    ui.visuals().widgets.inactive.bg_fill
                };
                if ui
                    .add_sized(BUTTON_SIZE, egui::Button::new(label).fill(fill))
                    .clicked()
                {
                    self.page = page;
                }
                ui.add_space(2.0);
            }
        });

        egui::Panel::bottom("actions")
            .exact_size(56.0)
            .show(root, |ui| {
                ui.add_space(4.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let busy = self.busy.load(Ordering::SeqCst);
                    let dirty = self.flag_dirty || self.gbs_dirty || self.settings_dirty;
                    if ui
                        .add_enabled(
                            !busy,
                            egui::Button::new("Save and Launch").min_size(ACTION_SIZE.into()),
                        )
                        .clicked()
                    {
                        self.save_all();
                        self.launch_job(LaunchMode::Player, false, self.pinned_version());
                    }
                    if ui
                        .add_enabled(
                            dirty && !busy,
                            egui::Button::new("Save all").min_size(ACTION_SIZE.into()),
                        )
                        .clicked()
                    {
                        self.save_all();
                    }
                    if ui
                        .add_enabled(
                            dirty && !busy,
                            egui::Button::new("Revert changes").min_size(ACTION_SIZE.into()),
                        )
                        .clicked()
                    {
                        self.revert_changes();
                    }
                    if ui
                        .add_sized(ACTION_SIZE, egui::Button::new("Exit"))
                        .clicked()
                    {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });

        egui::CentralPanel::default_margins().show(root, |ui| {
            if let Some(p) = progress::snapshot() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(&p.stage);
                    if p.total > 0 {
                        ui.add(
                            egui::ProgressBar::new(p.done as f32 / p.total.max(1) as f32)
                                .show_percentage()
                                .desired_width(220.0),
                        );
                        ui.label(format!("{}/{}", p.done, p.total));
                    }
                });
                ui.add_space(8.0);
            }
            // The CentralPanel already bounds available height, so the
            // ScrollArea fills exactly to the window bottom and scrolls
            // taller pages. Never cap it manually: a cap leaves a dead band
            // covering part of the page.
            egui::ScrollArea::vertical()
                .id_salt("page")
                .auto_shrink([false, false])
                .show(ui, |ui| match self.page {
                    Page::Home => self.home_page(ui),
                    Page::FastFlags => self.fast_flags_page(ui),
                    Page::Roblox => self.roblox_settings_page(ui),
                    Page::Settings => self.settings_page(ui),
                    Page::About => self.about_page(ui),
                });
        });
    }
}

impl RuststrapApp {
    fn home_page(&mut self, ui: &mut egui::Ui) {
        let linux_player = !cfg!(windows);
        ui.heading("Roblox");
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let busy = self.busy.load(Ordering::SeqCst);
            let launch = |ui: &mut egui::Ui, label: &str, mode: LaunchMode| {
                ui.add_enabled(!busy, egui::Button::new(label).min_size(BUTTON_SIZE.into()))
                    .clicked()
                    .then_some(mode)
            };
            if let Some(mode) = launch(ui, "Launch Player", LaunchMode::Player) {
                self.launch_job(mode, self.force_reinstall, self.pinned_version());
            }
            if let Some(mode) = launch(ui, "Launch Studio", LaunchMode::Studio) {
                self.launch_job(mode, self.force_reinstall, self.pinned_version());
            }
            ui.checkbox(&mut self.force_reinstall, "Reinstall");
        });
        ui.horizontal(|ui| {
            let hint = if cfg!(windows) {
                "Pin a client version (version-<hash>), blank = latest"
            } else {
                "Pin a Studio client version (version-<hash>), blank = latest"
            };
            ui.add(
                egui::TextEdit::singleline(&mut self.version_pin)
                    .hint_text(hint)
                    .desired_width(360.0),
            );
        });
        if linux_player {
            ui.label("Player is managed by Sober on Linux (Flatpak).");
        }

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);
        ui.heading("Versions");

        // a worker (install/uninstall) can hold the state for minutes; never
        // block a frame on it, just fall back to empty rows until it frees up
        let (player_guid, studio_guid) = match self.shared.try_lock() {
            Ok(guard) => (
                if cfg!(windows) {
                    crate::stock::installed_dir(crate::paths::get(), None)
                        .and_then(|dir| {
                            dir.file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                        })
                        .unwrap_or_default()
                } else {
                    guard.player_state.version_guid.clone()
                },
                guard.studio_state.version_guid.clone(),
            ),
            Err(_) => (String::new(), String::new()),
        };

        egui::Grid::new("versions")
            .striped(true)
            .num_columns(3)
            .show(ui, |ui| {
                let player_label = if linux_player {
                    "Player (Sober)"
                } else {
                    "Player"
                };
                if !linux_player {
                    version_row(
                        ui,
                        player_label,
                        &player_guid,
                        self.latest_player.as_deref(),
                    );
                }
                if cfg!(windows) || !linux_player {
                    version_row(ui, "Studio", &studio_guid, self.latest_studio.as_deref());
                }
            });

        if cfg!(windows)
            && ui
                .add_sized(BUTTON_SIZE, egui::Button::new("Check for updates"))
                .clicked()
        {
            self.check_versions_job();
        }
        if ui
            .add_sized(BUTTON_SIZE, egui::Button::new("Check version"))
            .clicked()
        {
            self.check_app_version_job();
        }
        if let Some(latest) = &self.app_latest {
            let current = env!("CARGO_PKG_VERSION");
            if latest.as_str() != current {
                ui.label(format!(
                    "Ruststrap update available: v{latest} (installed v{current})"
                ));
                if ui
                    .add_sized(BUTTON_SIZE, egui::Button::new("Open download page"))
                    .clicked()
                {
                    open_url("https://github.com/nemesisprojectstudios/Ruststrap/releases/latest");
                }
            } else {
                ui.weak("Ruststrap is up to date");
            }
        }

        if !cfg!(windows) {
            ui.add_space(16.0);
            ui.separator();
            ui.add_space(8.0);
            ui.heading("Sober");
            let busy = self.busy.load(Ordering::SeqCst);
            ui.label(if self.sober_installed {
                "Sober is installed"
            } else {
                "Sober is not installed"
            });
            let label = if self.sober_installed {
                "Update Sober"
            } else {
                "Install Sober"
            };
            if ui
                .add_enabled(!busy, egui::Button::new(label).min_size(BUTTON_SIZE.into()))
                .clicked()
            {
                self.sober_job(self.sober_installed);
            }
        }

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);
        ui.heading("Log");
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .max_height(260.0)
            .show(ui, |ui| {
                for line in logger::recent() {
                    ui.monospace(line);
                }
            });
    }

    fn fast_flags_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("Fast Flags");
        ui.label(
            "Applies to the Roblox client. On Linux, flag values are converted for Sober's config.",
        );
        ui.add_space(8.0);

        let json =
            serde_json::to_string_pretty(&Value::Object(self.fflags.clone())).unwrap_or_default();

        ui.horizontal_wrapped(|ui| {
            if ui
                .add_sized(BUTTON_SIZE, egui::Button::new("Add flag..."))
                .clicked()
            {
                self.dialog_error = None;
                self.add_dialog_open = true;
            }
            let have_selection = !self.flag_selected.is_empty();
            if ui
                .add_enabled(
                    have_selection,
                    egui::Button::new(format!("Delete selected ({})", self.flag_selected.len()))
                        .min_size(BUTTON_SIZE.into()),
                )
                .clicked()
            {
                for key in self.flag_selected.drain() {
                    self.fflags.remove(&key);
                    self.flag_edits.remove(&key);
                }
                self.flag_dirty = true;
                logger::info("Removed selected flags");
            }
            let delete_label = if self.confirm_delete_all {
                "Confirm delete all?"
            } else {
                "Delete all"
            };
            if ui
                .add_sized(BUTTON_SIZE, egui::Button::new(delete_label))
                .clicked()
            {
                if self.confirm_delete_all {
                    let count = self.fflags.len();
                    self.fflags.clear();
                    self.flag_edits.clear();
                    self.flag_selected.clear();
                    self.confirm_delete_all = false;
                    self.flag_dirty = true;
                    logger::info(&format!("Removed all {count} flags"));
                } else {
                    self.confirm_delete_all = true;
                    self.confirm_armed_at = Some(std::time::Instant::now());
                }
            }
            let save_enabled = self.flag_dirty && !self.busy.load(Ordering::SeqCst);
            if ui
                .add_enabled(
                    save_enabled,
                    egui::Button::new("Save flags").min_size(BUTTON_SIZE.into()),
                )
                .clicked()
            {
                self.save_flags();
            }
        });

        ui.horizontal_wrapped(|ui| {
            if ui
                .add_sized(BUTTON_SIZE, egui::Button::new("Import JSON"))
                .clicked()
            {
                self.dialog_error = None;
                self.import_text.clear();
                self.export_dialog_open = false;
                self.dialog_tab = AddTab::Json;
                self.add_dialog_open = true;
            }
            if ui
                .add_sized(BUTTON_SIZE, egui::Button::new("Export JSON"))
                .clicked()
            {
                self.export_text = json.clone();
                self.export_dialog_open = true;
            }
            if ui
                .add_sized(BUTTON_SIZE, egui::Button::new("Copy JSON"))
                .clicked()
            {
                ui.ctx().copy_text(json.clone());
                logger::info("Fast flags JSON copied to clipboard");
            }
        });

        ui.horizontal_wrapped(|ui| {
            if ui
                .add_sized(
                    BUTTON_SIZE,
                    egui::Button::new(if cfg!(windows) {
                        "Reload from file"
                    } else {
                        "Reload from Sober config"
                    }),
                )
                .clicked()
            {
                self.reload_flags();
                self.flag_dirty = false;
            }
            if ui
                .add_sized(BUTTON_SIZE, egui::Button::new("Clean non-allowlisted"))
                .clicked()
            {
                let before = self.fflags.len();
                self.fflags.retain(|key, _| fastflags::is_allowlisted(key));
                self.flag_edits
                    .retain(|key, _| self.fflags.contains_key(key));
                self.flag_selected
                    .retain(|key| self.fflags.contains_key(key));
                let removed = before.saturating_sub(self.fflags.len());
                if removed > 0 {
                    self.flag_dirty = true;
                }
                logger::info(&format!(
                    "Removed {removed} flags outside Roblox's allowlist"
                ));
            }
        });

        if self.confirm_delete_all
            && self
                .confirm_armed_at
                .is_some_and(|t| t.elapsed().as_secs_f32() > 5.0)
        {
            self.confirm_delete_all = false;
        }

        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            ui.label("Profiles:");
            let profiles = self.flag_profiles.clone();
            let selected = if self.profile_sel.is_empty() {
                "Select…".to_string()
            } else {
                self.profile_sel.clone()
            };
            egui::ComboBox::from_id_salt("flag-profile")
                .selected_text(selected)
                .width(170.0)
                .show_ui(ui, |ui| {
                    if profiles.is_empty() {
                        ui.weak("No profiles saved");
                    }
                    for name in profiles {
                        ui.selectable_value(&mut self.profile_sel, name.clone(), name);
                    }
                });
            ui.add(
                egui::TextEdit::singleline(&mut self.profile_name)
                    .hint_text("Profile name")
                    .desired_width(140.0),
            );
            if ui
                .add_sized(BUTTON_SIZE, egui::Button::new("Save profile"))
                .clicked()
            {
                let mut name = self.profile_name.trim().to_string();
                if name.is_empty() {
                    name = self.profile_sel.trim().to_string();
                }
                for (key, edit) in &self.flag_edits {
                    self.fflags
                        .insert(key.clone(), Value::String(edit.clone()));
                }
                self.flag_edits.clear();
                match fastflags::save_profile(paths::get(), &name, &self.fflags) {
                    Ok(stored) => {
                        logger::info(&format!("Saved flag profile '{stored}'"));
                        self.profile_sel = stored;
                        self.profile_name.clear();
                        self.refresh_profiles();
                    }
                    Err(e) => logger::error(&format!("Failed to save profile: {e}")),
                }
            }
            let loaded = !self.profile_sel.is_empty();
            if ui
                .add_enabled(
                    loaded,
                    egui::Button::new("Load profile").min_size(BUTTON_SIZE.into()),
                )
                .clicked()
            {
                match fastflags::load_profile(paths::get(), &self.profile_sel) {
                    Ok(flags) => {
                        self.fflags = flags;
                        self.flag_edits.clear();
                        self.flag_selected.clear();
                        self.flag_dirty = true;
                        logger::info(&format!(
                            "Loaded flag profile '{}' (press Save all to apply)",
                            self.profile_sel
                        ));
                    }
                    Err(e) => logger::error(&format!("Failed to load profile: {e}")),
                }
            }
            if ui
                .add_enabled(
                    loaded,
                    egui::Button::new("Delete profile").min_size(BUTTON_SIZE.into()),
                )
                .clicked()
            {
                match fastflags::delete_profile(paths::get(), &self.profile_sel) {
                    Ok(()) => {
                        logger::info(&format!(
                            "Deleted flag profile '{}'",
                            self.profile_sel
                        ));
                        self.profile_sel.clear();
                        self.refresh_profiles();
                    }
                    Err(e) => logger::error(&format!("Failed to delete profile: {e}")),
                }
            }
        });

        let query = self.flag_search.trim().to_lowercase();
        let rows: Vec<(String, String)> = self
            .fflags
            .iter()
            .filter(|(key, _)| query.is_empty() || key.to_lowercase().contains(&query))
            .map(|(key, value)| {
                let text = match value {
                    Value::String(inner) => inner.clone(),
                    other => other.to_string(),
                };
                (key.clone(), text)
            })
            .collect();

        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.flag_search)
                    .hint_text("Search")
                    .desired_width(160.0),
            );
            ui.label(format!("{} flags", rows.len()));
        });
        ui.add_space(4.0);

        if rows.is_empty() {
            ui.weak(if self.fflags.is_empty() {
                "No flags set."
            } else {
                "No flags match the search."
            });
        } else {
            let value_gap = 16.0;
            let value_w = 240.0;
            let row_h = ui.spacing().interact_size.y.max(24.0) + 6.0;
            let font = egui::TextStyle::Monospace.resolve(ui.style());
            let pointer = ui.input(|i| i.pointer.interact_pos());
            let mut click_on_row = false;
            for (index, (key, raw_value)) in rows.into_iter().enumerate() {
                let full_width = ui.available_width();
                let (row_rect, _) =
                    ui.allocate_exact_size(egui::vec2(full_width, row_h), egui::Sense::hover());
                let value_rect = egui::Rect::from_min_max(
                    egui::pos2(row_rect.right() - value_gap - value_w, row_rect.top()),
                    egui::pos2(row_rect.right() - value_gap, row_rect.bottom()),
                );
                let name_rect = egui::Rect::from_min_max(
                    egui::pos2(row_rect.left(), row_rect.top()),
                    egui::pos2(value_rect.left() - 8.0, row_rect.bottom()),
                );
                let painter = ui.painter();
                let selected = self.flag_selected.contains(&key);
                if selected {
                    painter.rect_filled(row_rect, 0.0, ui.visuals().selection.bg_fill);
                } else if index % 2 == 1 {
                    painter.rect_filled(row_rect, 0.0, ui.visuals().faint_bg_color);
                }
                if pointer.is_some_and(|p| row_rect.contains(p)) {
                    click_on_row = true;
                }

                if self.renaming.as_deref() == Some(key.as_str()) {
                    let edit = ui.put(
                        name_rect.shrink2(egui::vec2(0.0, 2.0)),
                        egui::TextEdit::singleline(&mut self.rename_buf)
                            .desired_width(name_rect.width()),
                    );
                    if edit.lost_focus() {
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            self.renaming = None;
                            self.rename_buf.clear();
                        } else {
                            self.finish_rename();
                        }
                    }
                } else {
                    let click = ui.interact(
                        name_rect,
                        ui.scope_id().with(("fflag-row", &key)),
                        egui::Sense::click(),
                    );
                    let allowlisted = fastflags::is_allowlisted(&key);
                    let click = if allowlisted {
                        click
                    } else {
                        click.on_hover_text(
                            "Not in Roblox's allowlist (September 2025); the client may ignore it",
                        )
                    };
                    painter.text(
                        name_rect.left_center(),
                        egui::Align2::LEFT_CENTER,
                        &if allowlisted {
                            key.clone()
                        } else {
                            format!("[ ! ] {key}")
                        },
                        font.clone(),
                        if selected {
                            ui.visuals().strong_text_color()
                        } else if allowlisted {
                            ui.visuals().text_color()
                        } else {
                            ui.visuals().warn_fg_color
                        },
                    );
                    if click.double_clicked() {
                        self.renaming = Some(key.clone());
                        self.rename_buf = key.clone();
                    } else if click.clicked() {
                        if ui.input(|i| i.modifiers.ctrl) {
                            if selected {
                                self.flag_selected.remove(&key);
                            } else {
                                self.flag_selected.insert(key.clone());
                            }
                        } else {
                            self.flag_selected.clear();
                            self.flag_selected.insert(key.clone());
                        }
                    }
                }

                let mut buf = self.flag_edits.get(&key).cloned().unwrap_or(raw_value);
                let edited = ui
                    .put(
                        value_rect,
                        egui::TextEdit::singleline(&mut buf).desired_width(value_w),
                    )
                    .changed();
                if edited {
                    self.flag_edits.insert(key.clone(), buf);
                    self.flag_dirty = true;
                }
            }

            if ui.input(|i| i.pointer.any_click()) && !click_on_row {
                self.flag_selected.clear();
            }
        }

        if self.add_dialog_open {
            let mut open = self.add_dialog_open;
            egui::Window::new("Add flag")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ui.ctx(), |ui| self.add_flag_dialog(ui));
            self.add_dialog_open = open;
        }
        if self.export_dialog_open {
            let mut open = self.export_dialog_open;
            egui::Window::new("Export JSON")
                .open(&mut open)
                .collapsible(false)
                .resizable(true)
                .show(ui.ctx(), |ui| {
                    ui.add_sized(
                        [460.0, 200.0],
                        egui::TextEdit::multiline(&mut self.export_text).interactive(false),
                    );
                    if ui
                        .add_sized(BUTTON_SIZE, egui::Button::new("Copy to clipboard"))
                        .clicked()
                    {
                        ui.ctx().copy_text(self.export_text.clone());
                        logger::info("Fast flags JSON copied to clipboard");
                    }
                });
            self.export_dialog_open = open;
        }
    }

    /// Froststrap's AddFastFlagDialog: a single flag or a pasted JSON map.
    fn add_flag_dialog(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.dialog_tab, AddTab::Single, "Add single");
            ui.selectable_value(&mut self.dialog_tab, AddTab::Json, "Import JSON");
        });
        ui.add_space(4.0);

        match self.dialog_tab {
            AddTab::Single => {
                egui::Grid::new("add_flag_fields")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Name");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.dialog_name)
                                .hint_text("FFlagExample")
                                .desired_width(280.0),
                        );
                        ui.end_row();
                        ui.label("Value");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.dialog_value)
                                .hint_text("True")
                                .desired_width(280.0),
                        );
                        ui.end_row();
                    });
                if ui
                    .add_sized(BUTTON_SIZE, egui::Button::new("Add flag"))
                    .clicked()
                {
                    let key = self.dialog_name.trim().to_string();
                    if key.is_empty() {
                        self.dialog_error = Some("Flag name is required.".into());
                    } else {
                        self.fflags
                            .insert(key.clone(), Value::String(self.dialog_value.clone()));
                        self.flag_edits.insert(key, self.dialog_value.clone());
                        self.dialog_name.clear();
                        self.dialog_value.clear();
                        self.dialog_error = None;
                        self.add_dialog_open = false;
                        self.flag_dirty = true;
                        logger::info("Flag added");
                    }
                }
                if !self.dialog_name.trim().is_empty()
                    && !fastflags::is_allowlisted(self.dialog_name.trim())
                {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "Not in Roblox's allowlist (September 2025); the client may ignore it.",
                    );
                }
            }
            AddTab::Json => {
                ui.add_sized(
                    [460.0, 200.0],
                    egui::TextEdit::multiline(&mut self.import_text)
                        .hint_text("{\"FFlagExample\": \"True\"}"),
                );
                if ui
                    .add_enabled(
                        self.picking.is_none(),
                        egui::Button::new("Choose file...").min_size(BUTTON_SIZE.into()),
                    )
                    .clicked()
                {
                    self.open_file_dialog(
                        FilePickTarget::FlagsImport,
                        "Import FastFlags",
                        "Fast flags JSON",
                        &["json"],
                        None,
                    );
                }
                if ui
                    .add_sized(BUTTON_SIZE, egui::Button::new("Import"))
                    .clicked()
                {
                    match serde_json::from_str::<Value>(&self.import_text) {
                        Ok(Value::Object(map)) => {
                            let count = map.len();
                            self.fflags.extend(map);
                            self.flag_edits.clear();
                            self.dialog_error = None;
                            self.add_dialog_open = false;
                            self.import_text.clear();
                            self.flag_dirty = true;
                            logger::info(&format!("Imported {count} flags"));
                        }
                        Ok(_) => {
                            self.dialog_error =
                                Some("Expected a JSON object of flag names to values.".into());
                        }
                        Err(e) => {
                            self.dialog_error = Some(format!("Invalid JSON: {e}"));
                        }
                    }
                }
            }
        }
        if let Some(error) = &self.dialog_error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
    }

    /// Source picker for importing another bootstrapper's configuration.
    fn import_dialog(&mut self, ui: &mut egui::Ui) {
        let mut sources: Vec<(&'static str, ImportSource)> =
            vec![("Froststrap", ImportSource::Froststrap)];
        #[cfg(windows)]
        {
            sources.push(("Bloxstrap", ImportSource::Bloxstrap));
            sources.push(("Fishstrap", ImportSource::Fishstrap));
        }
        sources.push(("Browse for a folder…", ImportSource::Browse));

        ui.label(
            "Imports FastFlags (all bootstrappers) and, from Froststrap, its \
             settings and saved flag profiles. Nothing is written until you press Save all.",
        );
        ui.add_space(4.0);
        if self.import_source >= sources.len() {
            self.import_source = 0;
        }
        for (index, (label, _)) in sources.iter().enumerate() {
            ui.radio_value(&mut self.import_source, index, *label);
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui
                .add_sized(BUTTON_SIZE, egui::Button::new("Import"))
                .clicked()
            {
                let source = sources[self.import_source].1;
                let label = sources[self.import_source].0;
                match source {
                    ImportSource::Browse => {
                        self.open_dir_dialog(
                            FilePickTarget::BootstrapperRoot,
                            "Locate the bootstrapper's data folder",
                        );
                    }
                    kind => match Self::bootstrapper_root(kind) {
                        Some(root) => {
                            self.import_summary = Some(match self.import_from_bootstrapper(&root)
                            {
                                Ok(summary) => summary,
                                Err(e) => format!("Import failed: {e}"),
                            });
                        }
                        None => {
                            self.import_summary = Some(format!(
                                "Could not find the {label} data folder; is it installed?"
                            ));
                        }
                    },
                }
            }
            if ui
                .add_sized(BUTTON_SIZE, egui::Button::new("Close"))
                .clicked()
            {
                self.import_dialog_open = false;
            }
        });
        if let Some(summary) = &self.import_summary {
            ui.add_space(4.0);
            ui.label(summary);
        }
    }

    /// Default data-folder location of the given bootstrapper.
    fn bootstrapper_root(kind: ImportSource) -> Option<std::path::PathBuf> {
        match kind {
            ImportSource::Froststrap => {
                let base = if cfg!(windows) {
                    std::env::var_os("APPDATA").map(std::path::PathBuf::from)
                } else {
                    Some(paths::home_dir().join(".config"))
                };
                base.map(|base| base.join("Froststrap"))
            }
            #[cfg(windows)]
            ImportSource::Bloxstrap | ImportSource::Fishstrap => {
                let name = match kind {
                    ImportSource::Bloxstrap => "Bloxstrap",
                    ImportSource::Fishstrap => "Fishstrap",
                    _ => return None,
                };
                std::env::var_os("LOCALAPPDATA")
                    .map(std::path::PathBuf::from)
                    .map(|base| base.join(name))
            }
            _ => None,
        }
    }

    /// Imports FastFlags (and, from Froststrap, settings plus saved flag
    /// profiles) from another bootstrapper's data folder into the edit
    /// caches. Nothing is persisted until the user saves.
    fn import_from_bootstrapper(&mut self, root: &std::path::Path) -> Result<String, String> {
        let mut flags_imported = 0usize;
        for path in [
            root.join("Modifications/clientsettings/ClientAppSettings.json"),
            root.join("Modifications/ClientSettings/ClientAppSettings.json"),
        ] {
            if self.try_import_flag_file(&path, &mut flags_imported) {
                break;
            }
        }

        let mut settings_imported = false;
        let settings_path = root.join("Settings.json");
        if settings_path.is_file() {
            match std::fs::read_to_string(&settings_path) {
                Ok(text) => match serde_json::from_str::<Value>(&text) {
                    Ok(Value::Object(obj)) => {
                        self.apply_froststrap_settings(&obj);
                        settings_imported = true;
                    }
                    _ => logger::warn("Froststrap Settings.json is not a JSON object; skipped"),
                },
                Err(e) => logger::warn(&format!("Could not read Froststrap Settings.json: {e}")),
            }
        }

        let mut profiles_imported = 0usize;
        let profiles_dir = root.join("SavedFlagProfiles");
        if profiles_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&profiles_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if !path.is_file() {
                        continue;
                    }
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let Ok(text) = std::fs::read_to_string(&path) else {
                        continue;
                    };
                    if let Some((map, _)) = parse_flags_json(&text) {
                        match fastflags::save_profile(paths::get(), &name, &map) {
                            Ok(_stored) => profiles_imported += 1,
                            Err(e) => logger::warn(&format!("Skipped profile {name}: {e}")),
                        }
                    }
                }
            }
            self.refresh_profiles();
        }

        if flags_imported == 0 && !settings_imported && profiles_imported == 0 {
            return Err(format!("nothing importable found in {}", root.display()));
        }
        self.flag_edits.clear();
        if flags_imported > 0 {
            self.flag_dirty = true;
        }
        let mut parts: Vec<String> = Vec::new();
        if flags_imported > 0 {
            parts.push(format!("{flags_imported} flags"));
        }
        if settings_imported {
            parts.push("settings".to_string());
        }
        if profiles_imported > 0 {
            parts.push(format!("{profiles_imported} flag profiles"));
        }
        Ok(format!(
            "Imported {} from {} (press Save all to apply)",
            parts.join(", "),
            root.display()
        ))
    }

    /// Merges a foreign flags JSON file into the current flag map.
    fn try_import_flag_file(&mut self, path: &std::path::Path, count: &mut usize) -> bool {
        let Ok(text) = std::fs::read_to_string(path) else {
            return false;
        };
        let Some((map, imported)) = parse_flags_json(&text) else {
            return false;
        };
        for (key, value) in map {
            self.fflags
                .insert(key, Value::String(fastflags::flag_value_to_string(&value)));
        }
        *count = imported;
        true
    }

    /// Maps Froststrap's Settings.json onto the settings edit cache.
    fn apply_froststrap_settings(&mut self, obj: &serde_json::Map<String, Value>) {
        let read_str = |key: &str| {
            obj.get(key)
                .and_then(Value::as_str)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let read_bool = |key: &str| obj.get(key).and_then(Value::as_bool);
        let edits = &mut self.settings_edits;
        if let Some(value) = read_str("PlayerChannel") {
            edits.player_channel = value.to_ascii_lowercase();
        }
        if let Some(value) = read_str("StudioChannel") {
            edits.studio_channel = value.to_ascii_lowercase();
        }
        if let Some(value) = read_str("RobloxDomain") {
            edits.roblox_domain = value;
        }
        if let Some(value) = read_str("RobloxTheme") {
            edits.theme = value.to_ascii_lowercase();
        }
        if let Some(value) = read_str("ChannelChangeMode") {
            edits.channel_change_mode = value.to_ascii_lowercase();
        }
        if let Some(value) = obj.get("MaxThreadDownload").and_then(Value::as_u64) {
            edits.max_threads = value.clamp(1, 32) as usize;
        }
        if let Some(value) = read_bool("UpdateRoblox") {
            edits.update_roblox = value;
        }
        if let Some(value) = read_bool("StaticDirectory") {
            edits.static_directory = value;
        }
        if let Some(value) = read_bool("EnableRobloxBackgroundApp") {
            edits.background_app = value;
        }
        if let Some(value) = read_str("CleanerOptions") {
            match value.as_str() {
                "Never" => edits.cleaner_days = 0,
                "OneDay" => edits.cleaner_days = 1,
                "OneWeek" => edits.cleaner_days = 7,
                "OneMonth" => edits.cleaner_days = 30,
                "TwoMonths" => edits.cleaner_days = 60,
                _ => {}
            }
        }
        self.settings_dirty = true;
    }

    /// Apply the live edits to the map and persist them to
    /// ClientAppSettings.json (Windows) or Sober's config.json (Linux).
    /// Enforces the in-memory caches (fast flags, Roblox settings, Sober
    /// settings, app settings) onto the on-disk files, overwriting anything
    /// edited on disk since the app loaded them.
    fn save_all(&mut self) {
        self.save_flags();
        // Roblox settings load lazily; until that page was opened the edits
        // are template defaults that must not overwrite the real file.
        if self.gbs_init {
            self.save_gbs();
        }
        self.save_sober_toggles();
        self.save_settings_edits();
    }

    /// Discards unsaved edits and reloads everything from disk.
    fn revert_changes(&mut self) {
        self.reload_flags();
        self.reload_gbs();
        let Ok(guard) = self.shared.try_lock() else {
            logger::error("A worker holds the state; retry reverting once it finishes");
            return;
        };
        self.settings_edits = SettingsEdits {
            player_channel: guard.settings.player_channel.clone(),
            studio_channel: guard.settings.studio_channel.clone(),
            channel_change_mode: guard.settings.channel_change_mode.clone(),
            cleaner_days: guard.settings.cleaner_days,
            max_threads: guard.settings.max_thread_download,
            update_roblox: guard.settings.update_roblox,
            background_app: guard.settings.background_app,
            auto_rejoin: guard.settings.auto_rejoin,
            roblox_domain: guard.settings.roblox_domain.clone(),
            static_directory: guard.settings.static_directory,
            theme: guard.settings.theme.clone(),
        };
        drop(guard);
        self.settings_dirty = false;
        logger::info("Unsaved changes reverted");
    }

    fn save_settings_edits(&mut self) {
        let Ok(mut guard) = self.shared.try_lock() else {
            logger::error("A worker holds the state; retry saving once it finishes");
            return;
        };
        let player_channel = self
            .settings_edits
            .player_channel
            .trim()
            .to_ascii_lowercase();
        let studio_channel = self
            .settings_edits
            .studio_channel
            .trim()
            .to_ascii_lowercase();
        guard.settings.player_channel = player_channel.clone();
        guard.settings.studio_channel = studio_channel;
        let mode = match self
            .settings_edits
            .channel_change_mode
            .to_ascii_lowercase()
            .as_str()
        {
            "prompt" => "prompt",
            "ignore" => "ignore",
            _ => "automatic",
        };
        self.settings_edits.channel_change_mode = mode.to_string();
        guard.settings.channel_change_mode = mode.to_string();
        guard.settings.update_roblox = self.settings_edits.update_roblox;
        guard.settings.background_app = self.settings_edits.background_app;
        guard.settings.auto_rejoin = self.settings_edits.auto_rejoin;
        guard.settings.max_thread_download = self.settings_edits.max_threads;
        guard.settings.cleaner_days = self.settings_edits.cleaner_days;
        if !self.settings_edits.roblox_domain.trim().is_empty() {
            guard.settings.roblox_domain = self.settings_edits.roblox_domain.trim().to_string();
        }
        guard.settings.static_directory = self.settings_edits.static_directory;
        if !self.settings_edits.theme.trim().is_empty() {
            guard.settings.theme = self.settings_edits.theme.trim().to_string();
        }
        guard.save_settings();
        drop(guard);
        #[cfg(not(windows))]
        {
            if let Err(e) = Self::set_sober_channel(&player_channel) {
                logger::error(&format!("Failed to sync Sober channel: {e}"));
            }
        }
        self.settings_dirty = false;
        logger::info("Settings saved");
    }

    /// Writes the cached Sober toggle states into Sober's config.json.
    /// Applies the double-click rename of a flag entry.
    fn finish_rename(&mut self) {
        let Some(old) = self.renaming.take() else {
            return;
        };
        let new_key = self.rename_buf.trim().to_string();
        self.rename_buf.clear();
        if new_key.is_empty() || new_key == old {
            return;
        }
        if self.fflags.contains_key(&new_key) {
            logger::error(&format!("Flag {new_key} already exists; rename cancelled"));
            return;
        }
        let has_edit = self.flag_edits.contains_key(&old);
        let value =
            self.flag_edits
                .get(&old)
                .cloned()
                .unwrap_or_else(|| match self.fflags.get(&old) {
                    Some(Value::String(inner)) => inner.clone(),
                    Some(other) => other.to_string(),
                    None => String::new(),
                });
        if !has_edit && self.fflags.remove(&old).is_none() {
            logger::error(&format!("Flag {old} not found; rename cancelled"));
            return;
        }
        self.flag_edits.remove(&old);
        self.flag_selected.remove(&old);
        self.flag_selected.insert(new_key.clone());
        self.fflags.insert(new_key, Value::String(value));
        self.flag_dirty = true;
        logger::info("Flag renamed");
    }

    fn save_sober_toggles(&mut self) {
        if cfg!(windows) {
            return;
        }
        let mut config = sober::load_config(paths::get());
        let toggles = self
            .sober_toggles
            .iter()
            .map(|(_, key, on)| (key.to_string(), *on))
            .collect::<Vec<_>>();
        let skipped = sober::apply_bool_toggles(&mut config, &toggles);
        for key in skipped {
            logger::warn(&format!(
                "Sober option {key} is not a boolean in config.json; left untouched"
            ));
        }
        if sober::coerce_bool_to_string(&mut config, "touch_mode", "off", "on") {
            logger::warn(
                "Repaired touch_mode in Sober config (a previous release wrote it as a boolean)",
            );
        }
        match sober::save_config(paths::get(), &config) {
            Ok(()) => logger::info("Sober settings saved"),
            Err(e) => logger::error(&format!("Failed to save Sober settings: {e}")),
        }
    }

    /// Sober reads the player channel from its own config.json; keep it in
    /// sync with the saved player channel.
    #[cfg(not(windows))]
    fn set_sober_channel(channel: &str) -> Result<(), String> {
        let mut config = sober::load_config(paths::get());
        config.insert("channel".to_string(), Value::String(channel.to_string()));
        sober::save_config(paths::get(), &config)
    }

    fn save_flags(&mut self) {
        for (key, edit) in &self.flag_edits {
            self.fflags.insert(key.clone(), Value::String(edit.clone()));
        }
        self.flag_edits.clear();
        self.flag_dirty = false;
        if cfg!(windows) {
            let path = fastflags::fflags_path(paths::get());
            match fastflags::save(&path, &self.fflags) {
                Ok(()) => logger::info("Fast flags saved"),
                Err(e) => logger::error(&format!("Failed to save fast flags: {e}")),
            }
        } else {
            // Sober reads flags from its config.json; write them there with typed values
            let mut config = sober::load_config(paths::get());
            let container = config
                .entry("fflags")
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            if !container.is_object() {
                *container = Value::Object(serde_json::Map::new());
            }
            let fflags = container.as_object_mut().expect("just ensured object");
            for (key, value) in &self.fflags {
                let as_str = match value {
                    Value::String(inner) => inner.clone(),
                    other => other.to_string(),
                };
                fflags.insert(key.clone(), fastflags::coerce_flag(&as_str));
            }
            match sober::save_config(paths::get(), &config) {
                Ok(()) => logger::info("Fast flags saved to Sober config"),
                Err(e) => logger::error(&format!("Failed to save fast flags: {e}")),
            }
        }
    }

    fn roblox_settings_page(&mut self, ui: &mut egui::Ui) {
        if !self.gbs_init {
            self.reload_gbs();
            self.gbs_init = true;
        }
        ui.heading("Roblox settings");
        if !self.gbs_exists {
            ui.weak("File not found yet; saving creates a fresh one.");
        }
        ui.add_space(8.0);

        ui.horizontal_wrapped(|ui| {
            let save_enabled = self.gbs_dirty && !self.busy.load(Ordering::SeqCst);
            if ui
                .add_enabled(
                    save_enabled,
                    egui::Button::new("Save changes").min_size(BUTTON_SIZE.into()),
                )
                .clicked()
            {
                self.save_gbs();
            }
            if ui
                .add_sized(BUTTON_SIZE, egui::Button::new("Reload"))
                .clicked()
            {
                self.reload_gbs();
            }
            if ui
                .add_enabled(
                    self.picking.is_none(),
                    egui::Button::new("Import XML...").min_size(BUTTON_SIZE.into()),
                )
                .clicked()
            {
                self.open_file_dialog(
                    FilePickTarget::RobloxImport,
                    "Import Roblox settings",
                    "Roblox settings",
                    &["xml"],
                    None,
                );
            }
            if ui
                .add_enabled(
                    self.picking.is_none(),
                    egui::Button::new("Export XML...").min_size(BUTTON_SIZE.into()),
                )
                .clicked()
            {
                self.open_file_dialog(
                    FilePickTarget::RobloxExport,
                    "Export Roblox settings",
                    "Roblox settings",
                    &["xml"],
                    Some("GlobalBasicSettings_13.xml"),
                );
            }
            if ui
                .add_sized(BUTTON_SIZE, egui::Button::new("Open file location"))
                .clicked()
            {
                open_folder(
                    gbs::file_location(paths::get())
                        .parent()
                        .map(std::path::Path::to_path_buf)
                        .unwrap_or_default(),
                );
            }
        });

        let mut read_only = self.gbs_read_only;
        if ui
            .checkbox(
                &mut read_only,
                "Read-only (Roblox forgets edited settings on exit without it)",
            )
            .changed()
        {
            match gbs::Gbs::set_read_only(read_only) {
                Ok(()) => {
                    self.gbs_read_only = read_only;
                    logger::info(&format!("Roblox settings file read-only: {read_only}"));
                }
                Err(e) => logger::error(&format!("Failed to set read-only: {e}")),
            }
        }
        let stubborn = self.gbs_edits.fullscreen
            || self.gbs_edits.max_quality
            || self.gbs_edits.vignette
            || self.gbs_edits.framerate_cap >= 1;
        if stubborn && !self.gbs_read_only {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "Enable Read-only for these settings to survive Roblox exiting.",
            );
        }
        if let Some(error) = &self.gbs_save_error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }

        tile_grid(
            ui,
            self,
            vec![
                (
                    "Graphics",
                    Box::new(|ui: &mut egui::Ui, g: &mut RuststrapApp| {
                        ui.horizontal(|ui| {
                            ui.label("Framerate limit (0 = none)");
                            if ui
                                .add(
                                    egui::DragValue::new(&mut g.gbs_edits.framerate_cap)
                                        .min_size(egui::vec2(84.0, 0.0)),
                                )
                                .changed()
                            {
                                g.gbs_dirty = true;
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Start quality level");
                            if ui
                                .add(egui::Slider::new(&mut g.gbs_edits.quality_level, 0..=10))
                                .changed()
                            {
                                g.gbs_dirty = true;
                            }
                        });
                        gbs_bool_row(
                            ui,
                            "Start in fullscreen",
                            &mut g.gbs_edits.fullscreen,
                            &mut g.gbs_dirty,
                        );
                        gbs_bool_row(
                            ui,
                            "Automatically create max quality",
                            &mut g.gbs_edits.max_quality,
                            &mut g.gbs_dirty,
                        );
                        gbs_bool_row(ui, "Vignette", &mut g.gbs_edits.vignette, &mut g.gbs_dirty);
                    }),
                ),
                (
                    "Audio",
                    Box::new(|ui: &mut egui::Ui, g: &mut RuststrapApp| {
                        for (label, field) in [
                            ("Master volume", "master_volume"),
                            ("Studio master volume", "studio_volume"),
                            ("Party voice volume", "party_voice"),
                            ("Voice chat volume", "voice_chat"),
                        ] {
                            g.audio_slider_row(ui, label, field);
                        }
                    }),
                ),
                (
                    "Input",
                    Box::new(|ui: &mut egui::Ui, g: &mut RuststrapApp| {
                        ui.horizontal(|ui| {
                            ui.label("Mouse sensitivity");
                            if ui
                                .add(
                                    egui::DragValue::new(&mut g.gbs_edits.mouse_sensitivity)
                                        .speed(0.1)
                                        .min_size(egui::vec2(84.0, 0.0)),
                                )
                                .changed()
                            {
                                g.gbs_dirty = true;
                            }
                        });
                        gbs_bool_row(
                            ui,
                            "Shift lock switch",
                            &mut g.gbs_edits.shift_lock,
                            &mut g.gbs_dirty,
                        );
                        g.vector_row(ui, "First person sensitivity", true);
                        g.vector_row(ui, "Third person sensitivity", false);
                        gbs_bool_row(
                            ui,
                            "Invert camera Y",
                            &mut g.gbs_edits.camera_y_inverted,
                            &mut g.gbs_dirty,
                        );
                    }),
                ),
                (
                    "Accessibility",
                    Box::new(|ui: &mut egui::Ui, g: &mut RuststrapApp| {
                        ui.horizontal(|ui| {
                            ui.label("UI transparency (0-1)");
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut g.gbs_edits.ui_transparency)
                                        .desired_width(80.0),
                                )
                                .changed()
                            {
                                g.gbs_dirty = true;
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Player list layout");
                            if ui
                                .add(
                                    egui::DragValue::new(&mut g.gbs_edits.player_list_layout)
                                        .range(0..=1),
                                )
                                .changed()
                            {
                                g.gbs_dirty = true;
                            }
                            ui.label("Text size");
                            if ui
                                .add(egui::DragValue::new(&mut g.gbs_edits.font_size).range(1..=4))
                                .changed()
                            {
                                g.gbs_dirty = true;
                            }
                        });
                        gbs_bool_row(
                            ui,
                            "Reduce motion",
                            &mut g.gbs_edits.reduced_motion,
                            &mut g.gbs_dirty,
                        );
                        gbs_bool_row(
                            ui,
                            "Show performance stats",
                            &mut g.gbs_edits.perf_stats,
                            &mut g.gbs_dirty,
                        );
                    }),
                ),
                (
                    "Modifications",
                    Box::new(|ui: &mut egui::Ui, g: &mut RuststrapApp| {
                        let mut disable_captures = g
                            .fflags
                            .get(fastflags::CAPTURE_DISABLE_FLAG)
                            .and_then(Value::as_str)
                            .map(|value| value.eq_ignore_ascii_case("false"))
                            .unwrap_or(false);
                        if ui
                            .checkbox(&mut disable_captures, "Disable screenshots & recording")
                            .changed()
                        {
                            if disable_captures {
                                g.fflags.insert(
                                    fastflags::CAPTURE_DISABLE_FLAG.to_string(),
                                    Value::String("False".to_string()),
                                );
                                logger::info(
                                    "Capture hotkeys disabled via FastFlag (best-effort: Roblox's allowlist currently ignores it)",
                                );
                            } else {
                                g.fflags.remove(fastflags::CAPTURE_DISABLE_FLAG);
                                logger::info("Capture hotkey flag removed");
                            }
                            g.flag_dirty = true;
                        }
                        ui.weak("(best-effort: Roblox's allowlist currently ignores this flag)");
                        g.settings_dirty |= ui
                            .checkbox(
                                &mut g.settings_edits.auto_rejoin,
                                "Auto-rejoin after a disconnect",
                            )
                            .changed();
                        ui.weak("Rejoins the last place up to 3 times after a disconnect.");
                    }),
                ),
                (
                    "Miscellaneous",
                    Box::new(|ui: &mut egui::Ui, g: &mut RuststrapApp| {
                        gbs_bool_row(
                            ui,
                            "Chat translation",
                            &mut g.gbs_edits.chat_translation,
                            &mut g.gbs_dirty,
                        );
                        gbs_bool_row(ui, "Enable VR", &mut g.gbs_edits.vr, &mut g.gbs_dirty);
                        ui.horizontal(|ui| {
                            ui.label("Haptic strength");
                            if ui
                                .add(
                                    egui::Slider::new(&mut g.gbs_edits.haptic_strength, 0.0..=1.0)
                                        .fixed_decimals(1),
                                )
                                .changed()
                            {
                                g.gbs_dirty = true;
                            }
                        });
                    }),
                ),
            ],
        );
    }

    fn audio_slider_row(&mut self, ui: &mut egui::Ui, label: &str, field: &str) {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label(label);
            let value = match field {
                "master_volume" => &mut self.gbs_edits.master_volume,
                "studio_volume" => &mut self.gbs_edits.studio_volume,
                "party_voice" => &mut self.gbs_edits.party_voice,
                _ => &mut self.gbs_edits.voice_chat,
            };
            changed = ui
                .add(egui::Slider::new(value, 0.0..=1.0).fixed_decimals(1))
                .changed();
        });
        if changed {
            self.gbs_dirty = true;
        }
    }

    /// Vector2 text pairs, edited as X/Y like Froststrap's GlobalSettingsPage.
    fn vector_row(&mut self, ui: &mut egui::Ui, label: &str, first_person: bool) {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label(label);
            let x = if first_person {
                &mut self.gbs_edits.fps_x
            } else {
                &mut self.gbs_edits.tps_x
            };
            changed |= ui
                .add(
                    egui::TextEdit::singleline(x)
                        .hint_text("X")
                        .desired_width(70.0),
                )
                .changed();
            let y = if first_person {
                &mut self.gbs_edits.fps_y
            } else {
                &mut self.gbs_edits.tps_y
            };
            changed |= ui
                .add(
                    egui::TextEdit::singleline(y)
                        .hint_text("Y")
                        .desired_width(70.0),
                )
                .changed();
        });
        if changed {
            self.gbs_dirty = true;
        }
    }

    /// Writes every exposed value back into the XML and persists it.
    fn save_gbs(&mut self) {
        for (label, raw) in [
            ("first person sensitivity X", self.gbs_edits.fps_x.trim()),
            ("first person sensitivity Y", self.gbs_edits.fps_y.trim()),
            ("third person sensitivity X", self.gbs_edits.tps_x.trim()),
            ("third person sensitivity Y", self.gbs_edits.tps_y.trim()),
        ] {
            if !raw.is_empty() && raw.replace(',', ".").parse::<f64>().is_err() {
                self.gbs_save_error = Some(format!("{label} is not a number"));
                return;
            }
        }
        if !self.gbs_edits.ui_transparency.trim().is_empty()
            && self
                .gbs_edits
                .ui_transparency
                .trim()
                .replace(',', ".")
                .parse::<f64>()
                .is_err()
        {
            self.gbs_save_error = Some("UI transparency is not a number".to_string());
            return;
        }

        let mut g = gbs::Gbs {
            raw: std::mem::take(&mut self.gbs.raw),
        };
        let e = &self.gbs_edits;
        let framerate = if e.framerate_cap < 1 {
            -1
        } else {
            e.framerate_cap
        };
        let result = (|| -> Result<(), String> {
            g.set("int", "FramerateCap", &framerate.to_string())?;
            g.set("token", "SavedQualityLevel", &e.quality_level.to_string())?;
            g.set_bool("Fullscreen", e.fullscreen)?;
            g.set_bool("MaxQualityEnabled", e.max_quality)?;
            g.set_bool("VignetteEnabled", e.vignette)?;
            g.set_bool("VignetteEnabledCustomOption", e.vignette)?;
            g.set_float("MasterVolume", e.master_volume)?;
            g.set_float("MasterVolumeStudio", e.studio_volume)?;
            g.set_float("PartyVoiceVolume", e.party_voice)?;
            g.set_float("VoiceChatVolume", e.voice_chat)?;
            g.set_float("MouseSensitivity", e.mouse_sensitivity)?;
            g.set("token", "ControlMode", if e.shift_lock { "1" } else { "0" })?;
            for (name, axis, value) in [
                ("MouseSensitivityFirstPerson", "X", e.fps_x.trim()),
                ("MouseSensitivityFirstPerson", "Y", e.fps_y.trim()),
                ("MouseSensitivityThirdPerson", "X", e.tps_x.trim()),
                ("MouseSensitivityThirdPerson", "Y", e.tps_y.trim()),
            ] {
                g.ensure_vector(name)?;
                if !value.is_empty() {
                    g.set_vector(name, axis, value)?;
                }
            }
            g.set_bool("CameraYInverted", e.camera_y_inverted)?;
            g.set_float("HapticStrength", e.haptic_strength)?;
            if !e.ui_transparency.trim().is_empty() {
                g.set("float", "PreferredTransparency", e.ui_transparency.trim())?;
            }
            g.set(
                "token",
                "PeoplePageLayout",
                &e.player_list_layout.to_string(),
            )?;
            g.set("token", "PreferredTextSize", &e.font_size.to_string())?;
            g.set_bool("ReducedMotion", e.reduced_motion)?;
            g.set_bool("PerformanceStatsVisible", e.perf_stats)?;
            g.set_bool("ChatTranslationEnabled", e.chat_translation)?;
            g.set_bool("VREnabled", e.vr)?;
            Ok(())
        })();

        self.gbs.raw = g.raw.clone();
        if let Err(e) = result {
            self.gbs_save_error = Some(e);
        } else {
            let doc = gbs::Gbs { raw: g.raw };
            match doc.save(paths::get()) {
                Ok(()) => {
                    self.gbs_dirty = false;
                    self.gbs_save_error = None;
                }
                Err(e) => self.gbs_save_error = Some(format!("Save failed: {e}")),
            }
        }
    }

    fn settings_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("Settings");
        ui.add_space(8.0);
        tile_grid(
            ui,
            self,
            vec![
                (
                    "Channels",
                    Box::new(|ui: &mut egui::Ui, g: &mut RuststrapApp| {
                        ui.horizontal(|ui| {
                            ui.label("Player");
                            g.settings_dirty |= channel_combo(
                                ui,
                                "player-channel",
                                &mut g.settings_edits.player_channel,
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label("Studio");
                            g.settings_dirty |= channel_combo(
                                ui,
                                "studio-channel",
                                &mut g.settings_edits.studio_channel,
                            );
                        });
                        if ui.button("Check Roblox channels").clicked() {
                            g.check_user_channels_job();
                        }
                        match (&g.user_channel_player, &g.user_channel_studio) {
                            (Some(player), Some(studio)) => {
                                ui.label(format!(
                                    "Your account uses: player '{player}', studio '{studio}'"
                                ));
                            }
                            (Some(player), None) => {
                                ui.label(format!("Your account uses: player '{player}'"));
                            }
                            (None, Some(studio)) => {
                                ui.label(format!("Your account uses: studio '{studio}'"));
                            }
                            _ => {}
                        }
                        ui.label(format!("Default: {DEFAULT_CHANNEL}"));
                        if ui.button("Show available channels").clicked() {
                            g.probe_channels_job();
                        }
                        if let Some(probes) = &g.channel_status {
                            for (channel, version) in probes {
                                match version {
                                    Some(version) => {
                                        ui.label(format!("{channel}: {version}"));
                                    }
                                    None => {
                                        ui.colored_label(
                                            ui.visuals().warn_fg_color,
                                            format!("{channel}: unavailable"),
                                        );
                                    }
                                }
                            }
                        }
                    }),
                ),
                (
                    "Behavior",
                    Box::new(|ui: &mut egui::Ui, g: &mut RuststrapApp| {
                        ui.horizontal(|ui| {
                            ui.label("Channel switch requests");
                            let mode = g.settings_edits.channel_change_mode.clone();
                            let changed = egui::ComboBox::from_id_salt("channel-change-mode")
                                .selected_text(mode)
                                .width(120.0)
                                .show_ui(ui, |ui| {
                                    for (text, value) in [
                                        ("Automatic", "automatic"),
                                        ("Prompt", "prompt"),
                                        ("Ignore", "ignore"),
                                    ] {
                                        ui.selectable_value(
                                            &mut g.settings_edits.channel_change_mode,
                                            value.to_string(),
                                            text,
                                        );
                                    }
                                })
                                .response
                                .changed();
                            g.settings_dirty |= changed;
                        });
                        g.settings_dirty |= ui
                            .checkbox(
                                &mut g.settings_edits.update_roblox,
                                "Update Roblox before launch",
                            )
                            .changed();
                        g.settings_dirty |= ui
                            .checkbox(
                                &mut g.settings_edits.background_app,
                                "Keep Roblox running in the background when closed",
                            )
                            .changed();
                        ui.horizontal(|ui| {
                            ui.label("Parallel downloads");
                            g.settings_dirty |= ui
                                .add(egui::Slider::new(&mut g.settings_edits.max_threads, 1..=32))
                                .changed();
                        });
                        ui.horizontal(|ui| {
                            ui.label("Cleaner (days old, 0 = off)");
                            g.settings_dirty |= ui
                                .add(egui::Slider::new(
                                    &mut g.settings_edits.cleaner_days,
                                    0..=90,
                                ))
                                .changed();
                        });
                    }),
                ),
            ],
        );

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui
                .add_sized(BUTTON_SIZE, egui::Button::new("Save settings"))
                .clicked()
            {
                self.save_settings_edits();
            }
            if ui
                .add_sized(
                    BUTTON_SIZE,
                    egui::Button::new("Import from another bootstrapper…"),
                )
                .clicked()
            {
                self.import_summary = None;
                self.import_dialog_open = true;
            }
        });

        if self.import_dialog_open {
            let mut open = self.import_dialog_open;
            egui::Window::new("Import from another bootstrapper")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ui.ctx(), |ui| self.import_dialog(ui));
            self.import_dialog_open = open;
        }

        if !cfg!(windows) {
            ui.add_space(16.0);
            ui.separator();
            ui.add_space(8.0);
            ui.heading("Sober settings");
            let busy = self.busy.load(Ordering::SeqCst);
            egui::Grid::new("sober_toggles")
                .num_columns(2)
                .show(ui, |ui| {
                    for index in 0..self.sober_toggles.len() {
                        ui.monospace(self.sober_toggles[index].0);
                        ui.checkbox(&mut self.sober_toggles[index].2, "");
                        ui.end_row();
                    }
                });
            let mut save_row = |ui: &mut egui::Ui| {
                if ui
                    .add_enabled(
                        !busy,
                        egui::Button::new("Save Sober settings").min_size(BUTTON_SIZE.into()),
                    )
                    .clicked()
                {
                    let toggles = self
                        .sober_toggles
                        .iter()
                        .map(|(_, key, on)| (key.to_string(), *on))
                        .collect::<Vec<_>>();
                    self.spawn_job("Saving Sober settings", move |_, _| {
                        let mut config = sober::load_config(paths::get());
                        for (key, on) in toggles {
                            config.insert(key, Value::Bool(on));
                        }
                        match sober::save_config(paths::get(), &config) {
                            Ok(()) => logger::info("Sober settings saved"),
                            Err(e) => logger::error(&format!("Failed to save Sober settings: {e}")),
                        }
                    });
                }
            };
            ui.horizontal(|ui| {
                save_row(ui);
                if ui
                    .add_sized(BUTTON_SIZE, egui::Button::new("Open file location"))
                    .clicked()
                {
                    let folder = sober::config_file(paths::get())
                        .parent()
                        .map(std::path::Path::to_path_buf)
                        .unwrap_or_default();
                    open_folder(folder);
                }
            });
        }

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);
        ui.heading("Maintenance");
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let busy = self.busy.load(Ordering::SeqCst);
            if ui
                .add_enabled(
                    !busy,
                    egui::Button::new("Clean now").min_size(BUTTON_SIZE.into()),
                )
                .clicked()
            {
                self.spawn_job("Cleaning", |shared, _| {
                    if let Ok(guard) = shared.lock() {
                        crate::cleaner::clean_now(paths::get(), guard.settings.cleaner_days.max(1));
                    }
                });
            }
            if ui
                .add_sized(BUTTON_SIZE, egui::Button::new("Open data folder"))
                .clicked()
            {
                open_folder(paths::get().data_root.clone());
            }
            if ui
                .add_sized(BUTTON_SIZE, egui::Button::new("Open modifications folder"))
                .clicked()
            {
                open_folder(paths::get().modifications.clone());
            }
            if ui
                .add_sized(BUTTON_SIZE, egui::Button::new("Open log folder"))
                .clicked()
            {
                open_folder(paths::get().logs.clone());
            }
        });

        ui.add_space(8.0);
        #[cfg(windows)]
        ui.horizontal(|ui| {
            if ui
                .add_sized(BUTTON_SIZE, egui::Button::new("Register protocols"))
                .clicked()
            {
                match std::env::current_exe() {
                    Ok(exe) => {
                        crate::registry::register_player(&exe);
                        crate::registry::register_studio(&exe);
                        logger::info("Protocols registered");
                    }
                    Err(e) => logger::error(&format!("Cannot locate executable: {e}")),
                }
            }
            if ui
                .add_sized(BUTTON_SIZE, egui::Button::new("Unregister protocols"))
                .clicked()
            {
                crate::registry::unregister_all();
                logger::info("Protocol registration removed");
            }
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let busy = self.busy.load(Ordering::SeqCst);
            ui.add_enabled(
                !busy,
                egui::Checkbox::new(
                    &mut self.confirm_uninstall,
                    "Uninstall Roblox (removes all versions and downloads)",
                ),
            );
            if self.confirm_uninstall
                && ui
                    .add_enabled(
                        !busy,
                        egui::Button::new(
                            egui::RichText::new("Uninstall now").color(egui::Color32::RED),
                        )
                        .min_size(BUTTON_SIZE.into()),
                    )
                    .clicked()
            {
                self.confirm_uninstall = false;
                let windows_stock = cfg!(windows);
                let pk = paths::get();
                self.spawn_job("Uninstalling", move |shared, _| {
                    let outcome = match shared.lock() {
                        Ok(mut guard) => {
                            if windows_stock {
                                crate::stock::uninstall(pk)
                            } else {
                                packages::uninstall(pk, &mut guard)
                            }
                        }
                        Err(e) => Err(format!("state lock unavailable: {e}")),
                    };
                    match outcome {
                        Ok(()) => logger::info("Uninstall finished"),
                        Err(e) => logger::error(&format!("Uninstall failed: {e}")),
                    }
                });
            }
        });
    }

    fn about_page(&self, ui: &mut egui::Ui) {
        ui.heading("Ruststrap");
        ui.add_space(8.0);
        ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
        ui.label(
            "A native Rust rewrite of Froststrap: installs, updates, patches and launches Roblox.",
        );
        ui.hyperlink_to(
            "Source",
            "https://github.com/nemesisprojectstudios/Ruststrap",
        );
        ui.hyperlink_to(
            "Froststrap (original)",
            "https://github.com/Froststrap/Froststrap",
        );
        ui.label("Licensed under MPL-2.0.");
    }
}

fn version_row(ui: &mut egui::Ui, label: &str, installed: &str, latest: Option<&str>) {
    ui.label(label);
    // display the hash without the shared "version-" prefix
    let hash = |guid: &str| guid.trim_start_matches("version-").to_string();
    let installed = if installed.is_empty() {
        "not installed".to_string()
    } else {
        hash(installed)
    };
    match latest {
        Some(latest) => {
            let latest = hash(latest);
            let up_to_date = installed.eq_ignore_ascii_case(&latest);
            if up_to_date {
                ui.label(
                    egui::RichText::new(&installed)
                        .color(egui::Color32::from_rgb(0x70, 0xC0, 0x70)),
                );
                ui.label("up to date");
            } else {
                ui.label(&installed);
                ui.label(
                    egui::RichText::new(format!("latest {latest}")).color(egui::Color32::YELLOW),
                );
            }
        }
        None => {
            ui.label(&installed);
            ui.label("");
        }
    }
    ui.end_row();
}

fn parse_flags_json(text: &str) -> Option<(serde_json::Map<String, Value>, usize)> {
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(map)) => {
            let count = map.len();
            Some((map, count))
        }
        _ => None,
    }
}

/// Lays out groups as equal-sized tiles, wrapping by the available width.
/// Overlong tile content scrolls inside its own tile.
/// Dropdown of Roblox's known deployment channels with a custom-entry
/// fallback. Returns true when the value changed.
fn channel_combo(ui: &mut egui::Ui, id_salt: &str, value: &mut String) -> bool {
    let known = KNOWN_CHANNELS
        .iter()
        .any(|channel| channel.eq_ignore_ascii_case(value.trim()));
    let selected = if value.trim().is_empty() {
        DEFAULT_CHANNEL.to_string()
    } else if known {
        value.clone()
    } else {
        "Custom…".to_string()
    };
    let combo_changed = egui::ComboBox::from_id_salt(id_salt)
        .selected_text(selected)
        .width(120.0)
        .show_ui(ui, |ui| {
            let mut changed = false;
            for channel in KNOWN_CHANNELS {
                changed |= ui
                    .selectable_value(value, channel.to_string(), channel)
                    .changed();
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Custom:");
                changed |= ui.text_edit_singleline(value).changed();
            });
            changed
        })
        .response
        .changed();
    if combo_changed {
        let text = value.trim().to_ascii_lowercase();
        *value = text;
    }
    combo_changed
}

fn tile_grid<A>(ui: &mut egui::Ui, app: &mut A, tiles: Vec<(&'static str, TileBody<A>)>) {
    let [width, height] = TILE_SIZE;
    let columns = ((ui.available_width() + TILE_GAP) / (width + TILE_GAP))
        .floor()
        .max(1.0) as usize;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = TILE_GAP;
        for row in tiles.chunks(columns) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = TILE_GAP;
                for (title, body) in row {
                    ui.allocate_ui(egui::vec2(width, height), |ui| {
                        ui.group(|ui| {
                            ui.set_min_size(egui::vec2(width - 16.0, height - 16.0));
                            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                                ui.spacing_mut().item_spacing.y = TILE_INNER_GAP;
                                ui.strong(*title);
                                egui::ScrollArea::vertical()
                                    .id_salt(format!("tile-{title}"))
                                    .auto_shrink([false, false])
                                    .show(ui, |ui| body(ui, app));
                            });
                        });
                    });
                }
            });
        }
    });
}

fn open_url(url: &str) {
    let result = if cfg!(windows) {
        std::process::Command::new("explorer")
            .arg(url)
            .spawn()
            .map(|_| ())
    } else {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map(|_| ())
    };
    if let Err(e) = result {
        logger::error(&format!("Cannot open {url}: {e}"));
    }
}

fn open_folder(path: std::path::PathBuf) {
    std::fs::create_dir_all(&path).ok();
    let result = if cfg!(windows) {
        std::process::Command::new("explorer")
            .arg(&path)
            .spawn()
            .map(|_| ())
    } else {
        std::process::Command::new("xdg-open")
            .arg(&path)
            .spawn()
            .map(|_| ())
    };
    if let Err(e) = result {
        logger::error(&format!("Cannot open {}: {e}", path.display()));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tile_scroll_area_fills_to_the_bottom() {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1000.0, 800.0),
            )),
            ..Default::default()
        };
        let out = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default_margins().show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.allocate_ui(egui::vec2(TILE_SIZE[0] + 10.0, TILE_SIZE[1]), |ui| {
                            ui.group(|ui| {
                                ui.set_min_size(egui::vec2(
                                    TILE_SIZE[0] - 16.0,
                                    TILE_SIZE[1] - 16.0,
                                ));
                                ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                                    ui.strong("T");
                                    egui::ScrollArea::vertical()
                                        .id_salt("tile-T")
                                        .auto_shrink([false, false])
                                        .show(ui, |ui| {
                                            for i in 0..60 {
                                                ui.label(format!("row-{i:03}"));
                                            }
                                        });
                                });
                            });
                        });
                    });
            });
        });
        let meshes = ctx.tessellate(out.shapes.clone(), 2.0);
        out.drop_without_applying_deltas();
        let bottom = TILE_SIZE[1] - 16.0;
        let deepest = meshes
            .iter()
            .filter_map(|cp| match &cp.primitive {
                egui::epaint::Primitive::Mesh(m) => {
                    m.vertices.iter().map(|v| v.pos.y).reduce(f32::max)
                }
                _ => None,
            })
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            deepest >= bottom,
            "tile content must paint down to the tile bottom (deepest {deepest}, want >= {bottom})"
        );
    }

    use super::*;

    #[test]
    fn tile_grid_lays_out_vertical_uniform_tiles() {
        let ctx = egui::Context::default();
        let observed: std::rc::Rc<std::cell::RefCell<Vec<(&'static str, egui::Rect)>>> =
            Default::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1000.0, 800.0),
            )),
            ..Default::default()
        };
        let out = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default_margins().show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let bodies: Vec<(&'static str, TileBody<()>)> = ["A", "B", "C", "D", "E"]
                            .into_iter()
                            .map(|name| {
                                let record = observed.clone();
                                (
                                    name,
                                    Box::new(move |ui: &mut egui::Ui, _app: &mut ()| {
                                        for row in ["one", "two", "three", "four"] {
                                            let rect = ui.label(format!("{name}-{row}")).rect;
                                            record.borrow_mut().push((name, rect));
                                        }
                                    }) as TileBody<()>,
                                )
                            })
                            .collect();
                        tile_grid(ui, &mut (), bodies);
                    });
            });
        });
        out.drop_without_applying_deltas();

        let labels = observed.borrow();
        assert_eq!(labels.len(), 20);
        for name in ["A", "B", "C", "D", "E"] {
            let rects: Vec<egui::Rect> = labels
                .iter()
                .filter(|(tile, _)| *tile == name)
                .map(|(_, r)| *r)
                .collect();
            assert_eq!(rects.len(), 4);
            for pair in rects.windows(2) {
                assert!(
                    pair[1].min.y > pair[0].min.y,
                    "tile {name} content must stack vertically"
                );
                assert!(
                    (pair[1].min.x - pair[0].min.x).abs() < 1.0,
                    "tile {name} content must share one column"
                );
            }
        }
        let first = |label: &str| {
            labels
                .iter()
                .find(|(tile, _)| *tile == &label[..1] && label.ends_with("one"))
                .map(|(_, r)| *r)
                .unwrap()
        };
        let (a, b, d) = (first("A-one"), first("B-one"), first("D-one"));
        let x_gap = b.min.x - a.min.x - TILE_SIZE[0];
        let y_gap = d.min.y - a.min.y - TILE_SIZE[1];
        assert!(
            (x_gap - y_gap).abs() <= 2.0,
            "tile gaps must match: {x_gap} vs {y_gap}"
        );
        assert!(
            a.min.x + TILE_SIZE[0] <= b.min.x,
            "tile B must tile to the side of tile A"
        );
        assert!(d.min.y > a.max.y, "tile D must tile below the first row");
        for (l1, r1) in labels.iter() {
            for (l2, r2) in labels.iter() {
                if l1 != l2 {
                    assert!(
                        !r1.intersects(*r2),
                        "labels from different tiles must not overlap"
                    );
                }
            }
        }
    }

    #[test]
    fn footer_bar_stays_pinned_across_passes() {
        let ctx = egui::Context::default();
        let mut bars = Vec::new();
        for (width, height) in [
            (800.0, 600.0),
            (800.0, 600.0),
            (1024.0, 768.0),
            (1024.0, 768.0),
        ] {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(width, height),
                )),
                ..Default::default()
            };
            let out = ctx.run_ui(input, |ui| {
                egui::Panel::left("nav").resizable(false).show(ui, |ui| {
                    ui.label("nav");
                });
                let bar = egui::Panel::bottom("actions")
                    .exact_size(56.0)
                    .show(ui, |ui| {
                        ui.add_space(4.0);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label("Save and Launch");
                        });
                    });
                let central = egui::CentralPanel::default_margins().show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for i in 0..50 {
                                ui.label(format!("row {i}"));
                            }
                        });
                });
                bars.push((bar.response.rect, central.response.rect, height));
            });
            out.drop_without_applying_deltas();
        }
        for (bar, central, height) in &bars {
            assert!(
                bar.max.y <= *height + 0.01,
                "footer bar must sit inside the window, got {:?} for {height}",
                bar
            );
            assert_eq!(bar.max.y, *height, "bar must touch the window bottom");
            assert!(
                central.max.y <= bar.min.y + 8.0 + 0.01,
                "central panel must end above the bar"
            );
        }
        assert_eq!(
            bars[0].0.size(),
            bars[1].0.size(),
            "bar height must not creep between passes"
        );
        assert_eq!(
            bars[2].0.size(),
            bars[3].0.size(),
            "bar height must not creep after resize"
        );
    }

    #[test]
    fn page_scroll_fills_central_panel() {
        let ctx = egui::Context::default();
        let observed: std::rc::Rc<std::cell::RefCell<Vec<(f32, f32, egui::Rect)>>> =
            Default::default();
        for window_height in [600.0, 900.0] {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(800.0, window_height),
                )),
                ..Default::default()
            };
            let record = observed.clone();
            let output = ctx.run_ui(input, |ui| {
                egui::Panel::left("nav").resizable(false).show(ui, |ui| {
                    ui.label("nav");
                });
                let resp = egui::CentralPanel::default_margins().show(ui, |ui| {
                    let scroll = egui::ScrollArea::vertical()
                        .id_salt("page")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for i in 0..200 {
                                ui.label(format!("item {i}"));
                            }
                        });
                    (scroll, ui.max_rect().height())
                });
                record.borrow_mut().push((
                    resp.response.rect.max.y,
                    resp.inner.1,
                    resp.inner.0.inner_rect,
                ));
            });
            output.drop_without_applying_deltas();
        }
        for (panel_bottom, inner_height, viewport) in observed.borrow().iter() {
            assert_eq!(
                panel_bottom - inner_height,
                16.0,
                "panel frame bottom must match the panel inner rect plus both margins"
            );
            assert!(
                viewport.max.y <= panel_bottom + 0.01,
                "scroll viewport must not spill below the panel"
            );
            assert!(
                (viewport.height() - inner_height).abs() < 32.0,
                "scroll viewport fills the panel: no dead band, got {} vs {}",
                viewport.height(),
                inner_height
            );
        }
    }

    #[test]
    fn page_content_scrolls_when_taller_than_window() {
        let ctx = egui::Context::default();
        let observed: std::rc::Rc<std::cell::RefCell<Option<(f32, f32)>>> = Default::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        };
        let record = observed.clone();
        let output = ctx.run_ui(input, |ui| {
            egui::Panel::left("nav").resizable(false).show(ui, |ui| {
                ui.label("nav");
            });
            egui::CentralPanel::default_margins().show(ui, |scroll_ui| {
                let scroll = egui::ScrollArea::vertical()
                    .id_salt("page")
                    .auto_shrink([false, false])
                    .show(scroll_ui, |ui| {
                        for i in 0..200 {
                            ui.label(format!("item {i}"));
                        }
                    });
                *record.borrow_mut() = Some((scroll.inner_rect.height(), scroll.content_size.y));
            });
        });
        output.drop_without_applying_deltas();
        let (viewport, content) = observed.borrow().expect("pass recorded");
        assert!(
            content > viewport + 50.0,
            "content ({content}) must exceed viewport ({viewport}) so it scrolls"
        );
    }
}
