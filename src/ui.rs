// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

//! egui-based UI mirroring Froststrap's feature set, in one window and one theme:
//! launch/update with progress and a live log, a fast flag editor, channel and
//! cleaner settings, folder shortcuts, uninstall and (Windows) protocol toggles.

use std::collections::HashMap;
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
}

type SharedCtx = Arc<Mutex<Ctx>>;

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
    new_flag_key: String,
    new_flag_value: String,
    settings_edits: SettingsEdits,
    confirm_uninstall: bool,
    force_reinstall: bool,
    latest_player: Option<String>,
    latest_studio: Option<String>,
}

#[derive(Default)]
struct SettingsEdits {
    player_channel: String,
    studio_channel: String,
    cleaner_days: u32,
    max_threads: usize,
    update_roblox: bool,
}

impl RuststrapApp {
    fn new(ctx: Ctx) -> RuststrapApp {
        let (tx, rx) = mpsc::channel();
        let fflags = fastflags::load_or_init(paths::get());
        let settings_edits = SettingsEdits {
            player_channel: ctx.settings.player_channel.clone(),
            studio_channel: ctx.settings.studio_channel.clone(),
            cleaner_days: ctx.settings.cleaner_days,
            max_threads: ctx.settings.max_thread_download,
            update_roblox: ctx.settings.update_roblox,
        };
        RuststrapApp {
            shared: Arc::new(Mutex::new(ctx)),
            tx,
            rx,
            busy: Arc::new(AtomicBool::new(false)),
            page: Page::Home,
            fflags,
            flag_edits: HashMap::new(),
            new_flag_key: String::new(),
            new_flag_value: String::new(),
            settings_edits,
            confirm_uninstall: false,
            force_reinstall: false,
            latest_player: None,
            latest_studio: None,
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
            work(&shared, &tx);
            progress::clear();
            busy.store(false, Ordering::SeqCst);
        });
    }

    fn launch_job(&mut self, mode: LaunchMode) {
        self.spawn_job("Launching Roblox", move |shared, _| {
            let outcome = {
                let mut guard = shared.lock().unwrap();
                let http = Http::new();
                let mut bootstrapper = Bootstrapper::new(
                    &http,
                    &mut guard,
                    mode,
                    String::new(),
                    None,
                    false,
                    false,
                    true,
                );
                bootstrapper.run()
            };
            match outcome {
                Ok(()) => logger::info("Launch finished"),
                Err(e) => logger::error(&format!("Launch failed: {e}")),
            }
        });
    }

    fn check_versions_job(&mut self) {
        self.spawn_job("Checking versions", move |shared, tx| {
            let http = Http::new();
            let guard = shared.lock().unwrap();
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
            let player = decode(false);
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
                if ui
                    .selectable_label(self.page == page, label)
                    .clicked()
                {
                    self.page = page;
                }
                ui.add_space(2.0);
            }
        });

        if let Some(p) = progress::snapshot() {
            egui::Panel::bottom("progress").show(root, |ui| {
                ui.add_space(6.0);
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
                ui.add_space(6.0);
            });
        }

        egui::Frame::central_panel(root.style()).show(root, |ui| match self.page {
            Page::Home => self.home_page(ui),
            Page::FastFlags => self.fast_flags_page(ui),
            Page::Settings => self.settings_page(ui),
            Page::About => self.about_page(ui),
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
            if ui.add_enabled(!busy, egui::Button::new("Launch Player")).clicked() {
                self.launch_job(LaunchMode::Player);
            }
            if ui.add_enabled(!busy, egui::Button::new("Launch Studio")).clicked() {
                self.launch_job(LaunchMode::Studio);
            }
            ui.checkbox(&mut self.force_reinstall, "Reinstall");
        });
        if linux_player {
            ui.label("Player is managed by Sober on Linux (Flatpak).");
        }

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);
        ui.heading("Versions");

        let (player_guid, studio_guid) = {
            let guard = self.shared.lock().unwrap();
            (
                guard.player_state.version_guid.clone(),
                guard.studio_state.version_guid.clone(),
            )
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

        if ui.button("Check for updates").clicked() {
            self.check_versions_job();
        }

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);
        ui.heading("Log");
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .auto_shrink([false, false])
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

        let mut changed = false;
        let mut to_remove: Vec<String> = Vec::new();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("fflags")
                    .striped(true)
                    .num_columns(3)
                    .min_col_width(160.0)
                    .show(ui, |ui| {
                        for key in self.fflags.keys().cloned().collect::<Vec<_>>() {
                            let display = if let Some(edit) = self.flag_edits.get(&key) {
                                edit.clone()
                            } else {
                                match self.fflags.get(&key) {
                                    Some(Value::String(s)) => s.clone(),
                                    Some(other) => other.to_string(),
                                    None => String::new(),
                                }
                            };
                            ui.monospace(&key);
                            let mut buf = display;
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut buf)
                                        .desired_width(240.0),
                                )
                                .changed()
                            {
                                self.flag_edits.insert(key.clone(), buf);
                                changed = true;
                            }
                            if ui.button("Remove").clicked() {
                                to_remove.push(key.clone());
                                changed = true;
                            }
                            ui.end_row();
                        }
                    });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.new_flag_key)
                        .on_hover_text("Flag name");
                    ui.text_edit_singleline(&mut self.new_flag_value)
                        .on_hover_text("Value");
                    if ui.button("Add flag").clicked() && !self.new_flag_key.trim().is_empty() {
                        let key = self.new_flag_key.trim().to_string();
                        self.fflags.insert(key.clone(), Value::String(self.new_flag_value.clone()));
                        self.flag_edits.insert(key, self.new_flag_value.clone());
                        self.new_flag_key.clear();
                        self.new_flag_value.clear();
                        changed = true;
                    }
                });
            });

        for key in to_remove {
            self.fflags.remove(&key);
            self.flag_edits.remove(&key);
        }
        if changed && ui.button("Save flags").clicked() {
            // apply live edits over the map
            for (key, edit) in &self.flag_edits {
                self.fflags.insert(key.clone(), Value::String(edit.clone()));
            }
            self.flag_edits.clear();
            let path = fastflags::fflags_path(paths::get());
            match fastflags::save(&path, &self.fflags) {
                Ok(()) => {
                    logger::info("Fast flags saved");
                    if !cfg!(windows) {
                        crate::sober::sync_fflags(paths::get(), &self.fflags);
                    }
                }
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
            if ui.button("Save settings").clicked() {
                let mut guard = self.shared.lock().unwrap();
                guard.settings.player_channel = self.settings_edits.player_channel.trim().to_ascii_lowercase();
                guard.settings.studio_channel = self.settings_edits.studio_channel.trim().to_ascii_lowercase();
                guard.settings.update_roblox = self.settings_edits.update_roblox;
                guard.settings.max_thread_download = self.settings_edits.max_threads;
                guard.settings.cleaner_days = self.settings_edits.cleaner_days;
                guard.save_settings();
                logger::info("Settings saved");
            }
        });

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);
        ui.heading("Maintenance");
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let busy = self.busy.load(Ordering::SeqCst);
            if ui.add_enabled(!busy, egui::Button::new("Clean now")).clicked() {
                self.spawn_job("Cleaning", |shared, _| {
                    let guard = shared.lock().unwrap();
                    crate::cleaner::clean_now(paths::get(), guard.settings.cleaner_days.max(1));
                });
            }
            if ui.button("Open data folder").clicked() {
                open_folder(paths::get().data_root.clone());
            }
            if ui.button("Open modifications folder").clicked() {
                open_folder(paths::get().modifications.clone());
            }
            if ui.button("Open log folder").clicked() {
                open_folder(paths::get().logs.clone());
            }
        });

        ui.add_space(8.0);
        #[cfg(windows)]
        ui.horizontal(|ui| {
            if ui.button("Register protocols").clicked() {
                match std::env::current_exe() {
                    Ok(exe) => {
                        crate::registry::register_player(&exe);
                        crate::registry::register_studio(&exe);
                        logger::info("Protocols registered");
                    }
                    Err(e) => logger::error(&format!("Cannot locate executable: {e}")),
                }
            }
            if ui.button("Unregister protocols").clicked() {
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
                    .add_enabled(!busy, egui::Button::new(egui::RichText::new("Uninstall now").color(egui::Color32::RED)))
                    .clicked()
            {
                self.confirm_uninstall = false;
                self.spawn_job("Uninstalling", |shared, _| {
                    let outcome = {
                        let mut guard = shared.lock().unwrap();
                        packages::uninstall(paths::get(), &mut guard)
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
