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
    Settings,
    About,
}

enum WorkerMsg {
    Versions { player: Option<String>, studio: Option<String> },
    SoberInstalled(bool),
}

#[derive(Clone, Copy, PartialEq)]
enum AddTab {
    Single,
    Json,
}

type SharedCtx = Arc<Mutex<Ctx>>;

/// Every button uses this size so the layout stays uniform.
const BUTTON_SIZE: [f32; 2] = [150.0, 32.0];

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
            });
            Ok(Box::new(RuststrapApp::new(ctx)))
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
    dialog_tab: AddTab,
    dialog_error: Option<String>,
    dialog_name: String,
    dialog_value: String,
    import_text: String,
    export_text: String,
    settings_edits: SettingsEdits,
    confirm_uninstall: bool,
    force_reinstall: bool,
    version_pin: String,
    latest_player: Option<String>,
    latest_studio: Option<String>,
    sober_installed: bool,
    sober_toggles: Vec<(&'static str, &'static str, bool)>,
}

/// Sober's own options (Froststrap's SoberSettingsManager preset keys).
const SOBER_TOGGLES: [(&str, &str); 11] = [
    ("Allow gamepad permission", "allow_gamepad_permission"),
    ("Enable GameMode", "enable_gamemode"),
    ("Enable HiDPI", "enable_hidpi"),
    ("Touch mode", "touch_mode"),
    ("Use console experience", "use_console_experience"),
    ("Use libsecret", "use_libsecret"),
    ("Use OpenGL", "use_opengl"),
    ("Server location indicator", "server_location_indicator_enabled"),
    ("Discord RPC", "discord_rpc_enabled"),
    ("Discord RPC join button", "discord_rpc_show_join_button"),
    ("Close on leave", "close_on_leave"),
];

#[derive(Default)]
struct SettingsEdits {
    player_channel: String,
    studio_channel: String,
    cleaner_days: u32,
    max_threads: usize,
    update_roblox: bool,
    background_app: bool,
}

impl RuststrapApp {
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
            cleaner_days: ctx.settings.cleaner_days,
            max_threads: ctx.settings.max_thread_download,
            update_roblox: ctx.settings.update_roblox,
            background_app: ctx.settings.background_app,
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
            dialog_tab: AddTab::Single,
            dialog_error: None,
            dialog_name: String::new(),
            dialog_value: String::new(),
            import_text: String::new(),
            export_text: String::new(),
            settings_edits,
            confirm_uninstall: false,
            force_reinstall: false,
            version_pin: String::new(),
            latest_player: None,
            latest_studio: None,
            sober_installed: !cfg!(windows) && sober::is_installed(),
            sober_toggles,
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
        logger::info("Fast flags reloaded");
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
        let title = if update { "Updating Sober" } else { "Installing Sober" };
        self.spawn_job(title, move |_, tx| {
            let outcome = if update { sober::update() } else { sober::ensure_installed() };
            match outcome {
                Ok(()) => {}
                Err(e) => logger::error(&format!("{title} failed: {e}")),
            }
            let _ = tx.send(WorkerMsg::SoberInstalled(sober::is_installed()));
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
}

impl eframe::App for RuststrapApp {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                WorkerMsg::Versions { player, studio } => {
                    self.latest_player = player;
                    self.latest_studio = studio;
                }
                WorkerMsg::SoberInstalled(installed) => self.sober_installed = installed,
            }
        }

        egui::Panel::left("nav").resizable(false).show(root, |ui| {
            ui.add_space(12.0);
            ui.heading("Ruststrap");
            ui.add_space(12.0);
            for (page, label) in [
                (Page::Home, "Home"),
                (Page::FastFlags, "Fast Flags"),
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

        egui::Frame::central_panel(root.style()).show(root, |ui| {
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
            // Cap explicitly against the live viewport rect: an unbounded
            // ScrollArea lets the page spill past the window edge with no
            // scrollbar, so taller pages were unreachable.
            let page_max = (ui.ctx().viewport_rect().height() - 96.0).max(160.0);
            egui::ScrollArea::vertical()
                .id_salt("page")
                .auto_shrink([false, false])
                .max_height(page_max)
                .show(ui, |ui| match self.page {
                    Page::Home => self.home_page(ui),
                    Page::FastFlags => self.fast_flags_page(ui),
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
                ui.add_enabled(
                    !busy,
                    egui::Button::new(label).min_size(BUTTON_SIZE.into()),
                )
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

        let (player_guid, studio_guid) = match self.shared.lock() {
            Ok(guard) => (
                guard.player_state.version_guid.clone(),
                guard.studio_state.version_guid.clone(),
            ),
            Err(_) => (String::new(), String::new()),
        };

        egui::Grid::new("versions").striped(true).num_columns(3).show(ui, |ui| {
            let player_label = if linux_player { "Player (Sober)" } else { "Player" };
            if !linux_player {
                version_row(ui, player_label, &player_guid, self.latest_player.as_deref());
            }
            if cfg!(windows) || !linux_player {
                version_row(ui, "Studio", &studio_guid, self.latest_studio.as_deref());
            }
        });

        if ui
            .add_sized(BUTTON_SIZE, egui::Button::new("Check for updates"))
            .clicked()
        {
            self.check_versions_job();
        }

        if !cfg!(windows) {
            ui.add_space(16.0);
            ui.separator();
            ui.add_space(8.0);
            ui.heading("Sober");
            let busy = self.busy.load(Ordering::SeqCst);
            ui.label(if self.sober_installed { "Sober is installed" } else { "Sober is not installed" });
            let label = if self.sober_installed { "Update Sober" } else { "Install Sober" };
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
        ui.label("Applies to the Roblox client (and to Sober on Linux with type coercion).");
        ui.add_space(8.0);

        let json = serde_json::to_string_pretty(&Value::Object(self.fflags.clone()))
            .unwrap_or_default();

        ui.horizontal_wrapped(|ui| {
            if ui.add_sized(BUTTON_SIZE, egui::Button::new("Add flag...")).clicked() {
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
            if ui.add_sized(BUTTON_SIZE, egui::Button::new(delete_label)).clicked() {
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
            if ui.add_sized(BUTTON_SIZE, egui::Button::new("Copy JSON")).clicked() {
                ui.ctx().copy_text(json.clone());
                logger::info("Fast flags JSON copied to clipboard");
            }
            if ui.add_sized(BUTTON_SIZE, egui::Button::new("Import JSON")).clicked() {
                self.dialog_error = None;
                self.import_text.clear();
                self.add_dialog_open = false;
                self.export_dialog_open = false;
                self.dialog_tab = AddTab::Json;
                self.add_dialog_open = true;
            }
            if ui.add_sized(BUTTON_SIZE, egui::Button::new("Export JSON")).clicked() {
                self.export_text = json.clone();
                self.export_dialog_open = true;
            }
            if ui
                .add_sized(BUTTON_SIZE, egui::Button::new(if cfg!(windows) {
                    "Reload from file"
                } else {
                    "Reload from Sober config"
                }))
                .clicked()
            {
                self.reload_flags();
                self.flag_dirty = false;
            }
            if self.confirm_delete_all
                && self
                    .confirm_armed_at
                    .is_some_and(|t| t.elapsed().as_secs_f32() > 5.0)
            {
                self.confirm_delete_all = false;
            }
            let save_enabled = self.flag_dirty && !self.busy.load(Ordering::SeqCst);
            if ui
                .add_enabled(save_enabled, egui::Button::new("Save flags").min_size(BUTTON_SIZE.into()))
                .clicked()
            {
                self.save_flags();
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.flag_search)
                    .hint_text("Search")
                    .desired_width(160.0),
            );
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

        ui.label(format!("{} flags", rows.len()));
        ui.add_space(4.0);

        if rows.is_empty() {
            ui.weak(if self.fflags.is_empty() { "No flags set." } else { "No flags match the search." });
        } else {
            egui::Grid::new("fflags")
                .striped(true)
                .num_columns(3)
                .min_col_width(160.0)
                .show(ui, |ui| {
                    for (key, value) in rows {
                        let mut selected = self.flag_selected.contains(&key);
                        if ui.checkbox(&mut selected, "").changed() {
                            if selected {
                                self.flag_selected.insert(key.clone());
                            } else {
                                self.flag_selected.remove(&key);
                            }
                        }
                        ui.monospace(&key);
                        let mut buf = value;
                        if ui
                            .add(egui::TextEdit::singleline(&mut buf).desired_width(240.0))
                            .changed()
                        {
                            self.flag_edits.insert(key.clone(), buf);
                            self.flag_dirty = true;
                        }
                        ui.end_row();
                    }
                });
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
                    if ui.add_sized(BUTTON_SIZE, egui::Button::new("Copy to clipboard")).clicked() {
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
                egui::Grid::new("add_flag_fields").num_columns(2).show(ui, |ui| {
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
                if ui.add_sized(BUTTON_SIZE, egui::Button::new("Add flag")).clicked() {
                    let key = self.dialog_name.trim().to_string();
                    if key.is_empty() {
                        self.dialog_error = Some("Flag name is required.".into());
                    } else {
                        self.fflags.insert(key.clone(), Value::String(self.dialog_value.clone()));
                        self.flag_edits.insert(key, self.dialog_value.clone());
                        self.dialog_name.clear();
                        self.dialog_value.clear();
                        self.dialog_error = None;
                        self.add_dialog_open = false;
                        self.flag_dirty = true;
                        logger::info("Flag added");
                    }
                }
            }
            AddTab::Json => {
                ui.add_sized(
                    [460.0, 200.0],
                    egui::TextEdit::multiline(&mut self.import_text)
                        .hint_text("{\"FFlagExample\": \"True\"}"),
                );
                if ui.add_sized(BUTTON_SIZE, egui::Button::new("Import")).clicked() {
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

    /// Apply the live edits to the map and persist them to
    /// ClientAppSettings.json (Windows) or Sober's config.json (Linux).
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

    fn settings_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("Settings");
        ui.add_space(8.0);
        ui.group(|ui| {
            ui.label("Channels");
            ui.horizontal(|ui| {
                ui.label("Player");
                ui.text_edit_singleline(&mut self.settings_edits.player_channel);
            });
            ui.horizontal(|ui| {
                ui.label("Studio");
                ui.text_edit_singleline(&mut self.settings_edits.studio_channel);
            });
            ui.label(format!("Default: {DEFAULT_CHANNEL}"));
        });

        ui.add_space(8.0);
        ui.group(|ui| {
            ui.label("Behavior");
            ui.checkbox(&mut self.settings_edits.update_roblox, "Update Roblox before launch");
            ui.checkbox(&mut self.settings_edits.background_app, "Keep Roblox running in the background when closed");
            ui.horizontal(|ui| {
                ui.label("Parallel downloads");
                ui.add(egui::Slider::new(&mut self.settings_edits.max_threads, 1..=32));
            });
            ui.horizontal(|ui| {
                ui.label("Cleaner (days old, 0 = off)");
                ui.add(egui::Slider::new(&mut self.settings_edits.cleaner_days, 0..=90));
            });
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui
                .add_sized(BUTTON_SIZE, egui::Button::new("Save settings"))
                .clicked()
            {
                let Ok(mut guard) = self.shared.lock() else {
                    logger::error("State file lock unavailable; settings not saved");
                    return;
                };
                guard.settings.player_channel = self.settings_edits.player_channel.trim().to_ascii_lowercase();
                guard.settings.studio_channel = self.settings_edits.studio_channel.trim().to_ascii_lowercase();
                guard.settings.update_roblox = self.settings_edits.update_roblox;
                guard.settings.background_app = self.settings_edits.background_app;
                guard.settings.max_thread_download = self.settings_edits.max_threads;
                guard.settings.cleaner_days = self.settings_edits.cleaner_days;
                guard.save_settings();
                logger::info("Settings saved");
            }
        });

        if !cfg!(windows) {
            ui.add_space(16.0);
            ui.separator();
            ui.add_space(8.0);
            ui.heading("Sober settings");
            let busy = self.busy.load(Ordering::SeqCst);
            egui::Grid::new("sober_toggles").num_columns(2).show(ui, |ui| {
                for index in 0..self.sober_toggles.len() {
                    ui.monospace(self.sober_toggles[index].0);
                    ui.checkbox(&mut self.sober_toggles[index].2, "");
                    ui.end_row();
                }
            });
            let mut save_row = |ui: &mut egui::Ui| {
            if ui
                .add_enabled(!busy, egui::Button::new("Save Sober settings").min_size(BUTTON_SIZE.into()))
                .clicked()
            {
                let toggles = self.sober_toggles
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
                .add_enabled(!busy, egui::Button::new("Clean now").min_size(BUTTON_SIZE.into()))
                .clicked()
            {
                self.spawn_job("Cleaning", |shared, _| {
                    if let Ok(guard) = shared.lock() {
                        crate::cleaner::clean_now(paths::get(), guard.settings.cleaner_days.max(1));
                    }
                });
            }
            if ui.add_sized(BUTTON_SIZE, egui::Button::new("Open data folder")).clicked() {
                open_folder(paths::get().data_root.clone());
            }
            if ui.add_sized(BUTTON_SIZE, egui::Button::new("Open modifications folder")).clicked() {
                open_folder(paths::get().modifications.clone());
            }
            if ui.add_sized(BUTTON_SIZE, egui::Button::new("Open log folder")).clicked() {
                open_folder(paths::get().logs.clone());
            }
        });

        ui.add_space(8.0);
        #[cfg(windows)]
        ui.horizontal(|ui| {
            if ui.add_sized(BUTTON_SIZE, egui::Button::new("Register protocols")).clicked() {
                match std::env::current_exe() {
                    Ok(exe) => {
                        crate::registry::register_player(&exe);
                        crate::registry::register_studio(&exe);
                        logger::info("Protocols registered");
                    }
                    Err(e) => logger::error(&format!("Cannot locate executable: {e}")),
                }
            }
            if ui.add_sized(BUTTON_SIZE, egui::Button::new("Unregister protocols")).clicked() {
                crate::registry::unregister_all();
                logger::info("Protocol registration removed");
            }
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let busy = self.busy.load(Ordering::SeqCst);
            ui.add_enabled(!busy, egui::Checkbox::new(&mut self.confirm_uninstall, "Uninstall Roblox (removes all versions and downloads)"));
            if self.confirm_uninstall
                && ui
                    .add_enabled(
                        !busy,
                        egui::Button::new(egui::RichText::new("Uninstall now").color(egui::Color32::RED))
                            .min_size(BUTTON_SIZE.into()),
                    )
                    .clicked()
            {
                self.confirm_uninstall = false;
                self.spawn_job("Uninstalling", |shared, _| {
                    let outcome = match shared.lock() {
                        Ok(mut guard) => packages::uninstall(paths::get(), &mut guard),
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
        ui.label("A native Rust rewrite of Froststrap: installs, updates, patches and launches Roblox.");
        ui.hyperlink_to("Source", "https://github.com/nemesisprojectstudios/Ruststrap");
        ui.hyperlink_to("Froststrap (original)", "https://github.com/Froststrap/Froststrap");
        ui.label("Licensed under MPL-2.0.");
    }
}

fn version_row(ui: &mut egui::Ui, label: &str, installed: &str, latest: Option<&str>) {
    ui.label(label);
    let installed = if installed.is_empty() { "not installed" } else { &installed[..8.min(installed.len())] };
    match latest {
        Some(latest) => {
            let up_to_date = installed.eq_ignore_ascii_case(&latest[..8.min(latest.len())]);
            if up_to_date {
                ui.label(egui::RichText::new(installed).color(egui::Color32::from_rgb(0x70, 0xC0, 0x70)));
                ui.label("up to date");
            } else {
                ui.label(installed);
                ui.label(egui::RichText::new(format!("latest {latest}")).color(egui::Color32::YELLOW));
            }
        }
        None => {
            ui.label(installed);
            ui.label("");
        }
    }
    ui.end_row();
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
