// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

use std::time::SystemTime;

use crate::deployment::{self, ChannelError};
use crate::http::Http;
use crate::launch;
use crate::lock::FileLock;
use crate::logger;
use crate::packages::{self};
use crate::paths::{self, Paths, DEFAULT_CHANNEL};
use crate::progress;
use crate::rejoin;
use crate::sober;
use crate::state::{ChannelChangeMode, Ctx, Settings};
use crate::watcher;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LaunchMode {
    None,
    Player,
    Studio,
}

pub struct Bootstrapper<'a> {
    http: &'a Http,
    paths: &'static Paths,
    ctx: &'a mut Ctx,
    mode: LaunchMode,
    launch_args: String,
    forced_version: Option<String>,
    channel_override: Option<String>,
    force: bool,
    no_launch: bool,
    no_wait: bool,
}

impl<'a> Bootstrapper<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        http: &'a Http,
        ctx: &'a mut Ctx,
        mode: LaunchMode,
        launch_args: String,
        forced_version: Option<String>,
        force: bool,
        no_launch: bool,
        no_wait: bool,
    ) -> Bootstrapper<'a> {
        Bootstrapper {
            http,
            paths: paths::get(),
            ctx,
            mode,
            launch_args,
            forced_version,
            channel_override: None,
            force,
            no_launch,
            no_wait,
        }
    }

    pub fn with_channel_override(mut self, channel: Option<String>) -> Self {
        self.channel_override = channel;
        self
    }

    fn is_studio(&self) -> bool {
        self.mode == LaunchMode::Studio
    }

    pub fn run(&mut self) -> Result<(), String> {
        logger::info("Running bootstrapper");

        if self.mode == LaunchMode::None {
            return Err("No launch mode set".to_string());
        }

        let channel = self.resolve_channel();

        let lock = FileLock::acquire(&self.paths.data_root, "Bootstrapper");
        logger::info("Lock acquired");

        // remember the joined place so a disconnected session can rejoin
        if self.mode == LaunchMode::Player && !self.launch_args.is_empty() {
            self.ctx.state.last_place_uri = self.launch_args.clone();
            self.ctx.save_state();
        }

        // On Linux the player's client is entirely managed by Sober: skip the
        // deployment check, version resolution, and all package machinery.
        if !cfg!(windows) && !self.is_studio() {
            return self.run_sober_flow();
        }

        // On Windows the player runs from the stock install the official
        // installer/WinGet maintains; only Studio uses Ruststrap's packages.
        if cfg!(windows) && !self.is_studio() {
            return self.run_windows_stock_flow(&channel);
        }

        progress::set("Fetching deployment info");
        let version = self.resolve_version(&channel)?;
        if !version.version.is_empty() {
            logger::info(&format!(
                "Roblox version {} ({})",
                version.version, version.version_guid
            ));
        }

        if cfg!(windows) || self.is_studio() {
            self.run_packaged_flow(&channel, &version.version_guid)?;
        } else {
            self.run_sober_flow()?;
        }

        drop(lock);
        Ok(())
    }

    fn run_packaged_flow(&mut self, channel: &str, version_guid: &str) -> Result<(), String> {
        let is_studio = self.is_studio();

        let base_url = deployment::resolve_base_url(self.http);
        let binary = packages::binary_type(is_studio);
        logger::info(&format!("Using binary type {binary}"));

        let outdated = self.ctx.distribution_state(is_studio).version_guid != version_guid;
        let needs =
            packages::must_upgrade(self.paths, self.ctx, is_studio, version_guid, self.force);

        if outdated || needs {
            packages::upgrade(
                self.http,
                self.paths,
                self.ctx,
                is_studio,
                channel,
                &base_url,
                version_guid,
            )?;
        } else {
            logger::info("Roblox is up to date");
        }

        let dir = packages::version_directory(self.paths, is_studio, version_guid);

        progress::set("Applying modifications");

        #[cfg(windows)]
        {
            let exe =
                std::env::current_exe().map_err(|e| format!("cannot resolve executable: {e}"))?;
            if is_studio {
                crate::registry::register_studio(&exe);
            } else {
                crate::registry::register_player(&exe);
            }
            crate::registry::register_client_location(is_studio, &dir.display().to_string());
            crate::registry::sync_channel(
                is_studio,
                &self.ctx.settings.roblox_domain,
                channel,
                deployment::is_default_channel(channel),
            );
        }

        packages::apply_modifications(self.paths, &dir);
        launch::apply_app_storage(self.paths, &self.ctx.settings);

        self.ctx.state.is_first_launch = false;
        self.ctx.save_state();

        if self.no_launch {
            logger::info("--no-launch given, skipping start");
            return Ok(());
        }

        progress::set("Launching Roblox");
        let Some(child) = launch::launch(self.paths, is_studio, &dir, &self.launch_args)? else {
            return Ok(());
        };

        if self.no_wait {
            logger::info("--no-wait given, leaving Roblox running");
            return Ok(());
        }

        progress::set("Waiting for Roblox to exit");
        watcher::wait_for_exit(child, packages::executable_name(is_studio));
        launch::apply_app_storage(self.paths, &self.ctx.settings);
        crate::cleaner::clean(self.paths, &self.ctx.settings);
        Ok(())
    }

    /// Windows player: mirrors the Sober flow. Roblox is installed and
    /// updated as the stock client (via winget), and Ruststrap only applies
    /// its modifications, registers the protocol handlers, and launches.
    fn run_windows_stock_flow(&mut self, channel: &str) -> Result<(), String> {
        if self.forced_version.is_some() {
            logger::warn("Pinned versions are not supported with the stock client; ignoring the pin");
            self.forced_version = None;
        }

        progress::set("Fetching deployment info");
        let version = match self.resolve_version(channel) {
            Ok(v) => v,
            Err(e) => {
                // offline or backend trouble: keep serving the installed client
                logger::warn(&format!(
                    "Version check failed ({e}); using the installed stock client"
                ));
                deployment::ClientVersion {
                    version: String::new(),
                    version_guid: String::new(),
                }
            }
        };
        if !version.version.is_empty() {
            logger::info(&format!(
                "Roblox version {} ({})",
                version.version, version.version_guid
            ));
        }

        progress::set("Checking Roblox install");
        let dir = crate::stock::ensure_stock_player(
            self.paths,
            if version.version_guid.is_empty() {
                None
            } else {
                Some(&version.version_guid)
            },
            self.force || self.ctx.state.force_reinstall,
        )?;

        #[cfg(windows)]
        {
            let exe =
                std::env::current_exe().map_err(|e| format!("cannot resolve executable: {e}"))?;
            crate::registry::register_player(&exe);
            crate::registry::register_client_location(false, &dir.display().to_string());
            crate::registry::sync_channel(
                false,
                &self.ctx.settings.roblox_domain,
                channel,
                deployment::is_default_channel(channel),
            );
        }

        progress::set("Applying modifications");
        packages::apply_modifications(self.paths, &dir);
        launch::apply_app_storage(self.paths, &self.ctx.settings);

        self.ctx.state.is_first_launch = false;
        self.ctx.save_state();

        if self.no_launch {
            logger::info("--no-launch given, skipping start");
            return Ok(());
        }

        progress::set("Launching Roblox");
        let session_start = SystemTime::now();
        let Some(child) = launch::launch(self.paths, false, &dir, &self.launch_args)? else {
            return Ok(());
        };
        let relaunch = move |paths: &'static Paths, uri: String| {
            launch::launch(paths, false, &dir, &uri)?
                .ok_or_else(|| "client did not start".to_string())
        };
        self.player_exit_tail(child, "RobloxPlayerBeta.exe", session_start, relaunch);
        Ok(())
    }

    /// Shared post-launch tail of both player flows: waits for the client,
    /// auto-rejoins on disconnect evidence, then runs the post-exit tasks.
    /// With --no-wait the whole tail runs detached so the caller returns
    /// immediately while the rejoin watcher keeps running.
    fn player_exit_tail(
        &mut self,
        child: std::process::Child,
        label: &'static str,
        session_start: SystemTime,
        relaunch: impl Fn(&'static Paths, String) -> Result<std::process::Child, String> + Send + 'static,
    ) {
        let paths = self.paths;
        let auto_rejoin = self.ctx.settings.auto_rejoin;
        let joined_uri = if self.launch_args.is_empty() {
            self.ctx.state.last_place_uri.clone()
        } else {
            self.launch_args.clone()
        };
        let has_uri = !joined_uri.is_empty();
        let settings: Settings = self.ctx.settings.clone();

        if self.no_wait {
            logger::info("--no-wait given, leaving Roblox running");
            if auto_rejoin && has_uri {
                std::thread::spawn(move || {
                    watcher::wait_for_exit(child, label);
                    let mut since = SystemTime::now();
                    let dirs = rejoin::client_log_dirs(paths);
                    let mut attempt = || relaunch(paths, joined_uri.clone());
                    rejoin::reconnect_loop(
                        true,
                        true,
                        &dirs,
                        label,
                        &mut since,
                        rejoin::REJOIN_DELAY,
                        &mut attempt,
                    );
                    launch::apply_app_storage(paths, &settings);
                    crate::cleaner::clean(paths, &settings);
                });
            }
            return;
        }

        progress::set("Waiting for Roblox to exit");
        watcher::wait_for_exit(child, label);
        if auto_rejoin && has_uri {
            let mut since = session_start;
            let dirs = rejoin::client_log_dirs(paths);
            let mut attempt = || relaunch(paths, joined_uri.clone());
            rejoin::reconnect_loop(
                true,
                true,
                &dirs,
                label,
                &mut since,
                rejoin::REJOIN_DELAY,
                &mut attempt,
            );
        }
        launch::apply_app_storage(paths, &settings);
        crate::cleaner::clean(paths, &settings);
    }

    fn run_sober_flow(&mut self) -> Result<(), String> {
        logger::info("Linux (Player): Sober manages Roblox, skipping deployment check");
        sober::ensure_installed()?;
        sober::kill();

        self.ctx.state.is_first_launch = false;
        self.ctx.save_state();

        if self.no_launch {
            logger::info("--no-launch given, skipping start");
            return Ok(());
        }

        let session_start = SystemTime::now();
        let child = sober::launch(&self.launch_args)?;
        let relaunch = |_: &'static Paths, uri: String| sober::launch(&uri);
        self.player_exit_tail(child, "sober", session_start, relaunch);
        Ok(())
    }

    /// Explicit --channel wins (enrolled persistently), then a channel inside
    /// the launch URI (subject to the user's channel-change mode), then the
    /// saved per-mode channel.
    fn resolve_channel(&mut self) -> String {
        let from_uri = deployment::channel_from_launch_args(&self.launch_args);
        if let Some(requested) = self.channel_override.clone() {
            self.enroll_channel(&requested);
            return requested;
        }
        let saved = if self.is_studio() {
            &self.ctx.settings.studio_channel
        } else {
            &self.ctx.settings.player_channel
        };
        let saved = if saved.trim().is_empty() {
            DEFAULT_CHANNEL.to_string()
        } else {
            saved.clone()
        };
        match from_uri {
            Some(requested) if !requested.eq_ignore_ascii_case(&saved) => {
                match self.ctx.settings.channel_mode() {
                    ChannelChangeMode::Automatic => {
                        logger::info(&format!(
                            "Launch requested channel '{requested}', enrolling automatically"
                        ));
                        self.enroll_channel(&requested);
                        requested
                    }
                    ChannelChangeMode::Prompt => {
                        if Self::prompt_channel_switch(&saved, &requested) {
                            self.enroll_channel(&requested);
                            requested
                        } else {
                            logger::info(&format!(
                                "Channel switch to '{requested}' declined, staying on '{saved}'"
                            ));
                            saved
                        }
                    }
                    ChannelChangeMode::Ignore => {
                        logger::info(&format!(
                            "Launch requested channel '{requested}', blocked by settings"
                        ));
                        saved
                    }
                }
            }
            Some(requested) => requested,
            None => saved,
        }
    }

    /// Persists the channel for the active launch mode.
    fn enroll_channel(&mut self, channel: &str) {
        logger::info(&format!("Enrolling into channel '{channel}'"));
        if self.is_studio() {
            self.ctx.settings.studio_channel = channel.to_string();
        } else {
            self.ctx.settings.player_channel = channel.to_string();
        }
        self.ctx.save_settings();
    }

    fn prompt_channel_switch(from: &str, to: &str) -> bool {
        rfd::MessageDialog::new()
            .set_title("Ruststrap")
            .set_description(format!(
                "The game requested Roblox channel '{to}'. Switch from '{from}'?"
            ))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show()
            == rfd::MessageDialogResult::Yes
    }

    fn resolve_version(&mut self, channel: &str) -> Result<deployment::ClientVersion, String> {
        if let Some(guid) = self.forced_version.take() {
            logger::info(&format!("Version pinned by argument: {guid}"));
            return Ok(deployment::ClientVersion {
                version: String::new(),
                version_guid: guid,
            });
        }

        let binary = packages::binary_type(self.is_studio());
        match deployment::get_client_version(self.http, &self.ctx.settings, channel, binary) {
            Ok(info) => Ok(info),
            Err(ChannelError::Invalid(code)) => {
                logger::warn(&format!(
                    "Reverting enrolled channel to {DEFAULT_CHANNEL}: {channel} is not usable ({code})"
                ));
                if self.is_studio() {
                    self.ctx.settings.studio_channel = DEFAULT_CHANNEL.to_string();
                } else {
                    self.ctx.settings.player_channel = DEFAULT_CHANNEL.to_string();
                }
                self.ctx.save_settings();
                deployment::get_client_version(
                    self.http,
                    &self.ctx.settings,
                    DEFAULT_CHANNEL,
                    binary,
                )
                .map_err(|e| format!("{e}"))
            }
            Err(ChannelError::Network(e)) => Err(format!("Failed to get deployment info: {e}")),
        }
    }
}
