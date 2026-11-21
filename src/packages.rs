// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::deployment;
use crate::http::{fast_hash_file, Http};
use crate::logger;
use crate::manifest::{
    copy_dir_overwrite, extract_zip, package_map, parse_manifest, remove_dir_forced, Package,
};
use crate::paths::Paths;
use crate::state::{load, Ctx, DistributionState, Settings};
use crate::{fastflags, processes};

pub const SOBER_DIR_NAME: &str = "Sober";

pub fn binary_type(is_studio: bool) -> &'static str {
    match (is_studio, cfg!(windows)) {
        (true, _) => "WindowsStudio64",
        (false, true) => "WindowsPlayer",
        // Linux player never downloads packages: Sober fetches its own client.
        _ => SOBER_DIR_NAME,
    }
}

pub fn executable_name(is_studio: bool) -> &'static str {
    if is_studio {
        "RobloxStudioBeta.exe"
    } else {
        "RobloxPlayerBeta.exe"
    }
}

pub fn version_directory(paths: &Paths, is_studio: bool, version_guid: &str) -> PathBuf {
    if load::<Settings>(&paths.config_root.join("Settings.json")).static_directory {
        paths.versions.join(binary_type(is_studio))
    } else {
        paths.versions.join(version_guid)
    }
}

/// A version directory counts as installed only when the client executable is present.
pub fn is_installed(version_dir: &Path, is_studio: bool) -> bool {
    version_dir.join(executable_name(is_studio)).exists()
}

/// Everything that invalidates an existing install and forces a clean one.
pub fn must_upgrade(
    paths: &Paths,
    ctx: &Ctx,
    is_studio: bool,
    version_guid: &str,
    force: bool,
) -> bool {
    if force || ctx.state.force_reinstall {
        return true;
    }
    let dir = version_directory(paths, is_studio, version_guid);
    if !is_installed(&dir, is_studio) {
        return true;
    }
    // Roblox's Miraculous "WebView2 is mandatory" era: an incomplete extraction
    // of these two files means the client cannot start.
    cfg!(windows)
        && !is_studio
        && (!dir.join("WebView2Loader.dll").exists() || !dir.join("RobloxPlayerBeta.dll").exists())
}

/// Full package install/upgrade flow (Windows player/studio, Linux studio).
pub fn upgrade(
    http: &Http,
    paths: &'static Paths,
    ctx: &mut Ctx,
    is_studio: bool,
    channel: &str,
    base_url: &str,
    version_guid: &str,
) -> Result<(), String> {
    if !ctx.settings.update_roblox {
        logger::info("Upgrading disabled, cancelling the upgrade");
        if !is_installed(
            &version_directory(paths, is_studio, version_guid),
            is_studio,
        ) {
            return Err("UpdateRoblox is disabled and no client is installed".to_string());
        }
        return Ok(());
    }

    logger::info(&format!(
        "{} Roblox ({})",
        if ctx.distribution_state(is_studio).version_guid.is_empty() {
            "Installing"
        } else {
            "Upgrading"
        },
        version_guid
    ));

    let target = version_directory(paths, is_studio, version_guid);
    let _ = std::fs::create_dir_all(&paths.downloads);
    let _ = std::fs::create_dir_all(&target);

    // nothing may hold files open while packages are overwritten
    if cfg!(windows) {
        processes::kill_images(&[
            processes::roblox_player_image(),
            processes::roblox_crash_handler_image(),
            "RobloxStudioBeta.exe",
        ]);
    } else {
        processes::kill_images(&["RobloxStudioBeta.exe"]);
    }

    // forced reinstalls always start from a fully clean directory
    remove_dir_forced(&target);
    let _ = std::fs::create_dir_all(&target);

    let manifest_text = http
        .get_string(&deployment::location(
            base_url,
            channel,
            &format!("/{version_guid}-rbxPkgManifest.txt"),
        ))
        .map_err(|e| format!("Failed to download package manifest: {e}"))?;
    let packages = parse_manifest(&manifest_text)?;
    logger::info(&format!(
        "Fetched package manifest ({} packages)",
        packages.len()
    ));

    let dir_map = Arc::new(package_map(is_studio));
    let target = Arc::new(target);

    // largest first so slow packages start earliest
    let mut queue: Vec<Package> = packages.clone();
    queue.sort_by_key(|pkg| std::cmp::Reverse(pkg.packed_size));

    let queue = Arc::new(Mutex::new(queue));
    let failed_count = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let finished_count = Arc::new(std::sync::atomic::AtomicU64::new(0));
    crate::progress::set_counted("Downloading packages", 0, packages.len() as u64);
    let failures: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let worker_count = ctx
        .settings
        .max_thread_download
        .clamp(1, 32)
        .min(queue.lock().map(|q| q.len()).unwrap_or(1).max(1));

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            let queue = queue.clone();
            let failures = failures.clone();
            let dir_map = dir_map.clone();
            let target = target.clone();
            let failed_count = failed_count.clone();
            let finished_count = finished_count.clone();
            scope.spawn(move || {
                while let Some(pkg) = queue.lock().ok().and_then(|mut q| q.pop()) {
                    let result = (|| -> Result<(), String> {
                        let download_path =
                            download_package(http, paths, &pkg, base_url, channel, version_guid)?;
                        let Some(&dir) = dir_map.get(pkg.name.as_str()) else {
                            logger::warn(&format!(
                                "{} not found in package map, skipping",
                                pkg.name
                            ));
                            return Ok(());
                        };
                        let target_dir = target.join(dir);
                        let _ = std::fs::create_dir_all(&target_dir);
                        extract_zip(&download_path, &target_dir)?;
                        Ok(())
                    })();

                    match result {
                        Ok(()) => logger::info(&format!("Package ready: {}", pkg.name)),
                        Err(e) => {
                            logger::error(&format!("Failed to install {}: {e}", pkg.name));
                            if let Ok(mut list) = failures.lock() {
                                list.push(format!("{}: {e}", pkg.name));
                            }
                            failed_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                    let done =
                        finished_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    let remaining = queue.lock().ok().map(|q| q.len()).unwrap_or(0) as u64;
                    crate::progress::set_counted("Downloading packages", done, done + remaining);
                }
            });
        }
    });

    let failures = failures.lock().ok().map(|f| f.clone()).unwrap_or_default();
    if !failures.is_empty() {
        return Err(format!(
            "{} package(s) failed to install: {}",
            failures.len(),
            failures.join("; ")
        ));
    }

    {
        let state = ctx.distribution_state_mut(is_studio);
        state.version_guid = version_guid.to_string();
        state.package_hashes = packages
            .iter()
            .map(|p| (p.name.clone(), p.signature.clone()))
            .collect();
    }

    cleanup_versions(paths, ctx, version_guid);
    cleanup_downloads(paths, ctx);

    ctx.save_distribution_state(is_studio);
    logger::info("Upgrade complete");
    Ok(())
}

fn download_package(
    http: &Http,
    paths: &Paths,
    pkg: &Package,
    base_url: &str,
    channel: &str,
    version_guid: &str,
) -> Result<PathBuf, String> {
    let download_path = paths.downloads.join(&pkg.signature);
    if download_path.exists() {
        match fast_hash_file(&download_path) {
            Some(hash) if hash.eq_ignore_ascii_case(&pkg.signature) => {
                logger::info(&format!("{} already downloaded, skipping", pkg.name));
                return Ok(download_path);
            }
            _ => {
                logger::warn(&format!(
                    "Cached package {} is corrupted, deleting",
                    pkg.signature
                ));
                let _ = std::fs::remove_file(&download_path);
            }
        }
    }

    // cheat like Froststrap: the stock bootstrapper caches packages under the same MD5 name
    #[cfg(windows)]
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let roblox_package = PathBuf::from(local)
            .join("Roblox")
            .join("Downloads")
            .join(&pkg.signature);
        if roblox_package.exists()
            && fast_hash_file(&roblox_package)
                .is_some_and(|hash| hash.eq_ignore_ascii_case(&pkg.signature))
        {
            logger::info(&format!(
                "Found existing copy of {} in stock Roblox downloads, copying",
                pkg.name
            ));
            std::fs::copy(&roblox_package, &download_path).map_err(|e| format!("copy: {e}"))?;
            return Ok(download_path);
        }
    }

    let url = deployment::location(base_url, channel, &format!("/{version_guid}-{}", pkg.name));
    http.download(&url, &download_path, Some(&pkg.signature))?;
    Ok(download_path)
}

/// Removes every installed version and cached download, resetting distribution state.
/// Sober (the Linux player client) is flatpak-managed and intentionally untouched.
pub fn uninstall(paths: &Paths, ctx: &mut Ctx) -> Result<(), String> {
    processes::kill_images(&[executable_name(false), executable_name(true)]);
    for dir in [&paths.versions, &paths.downloads] {
        if dir.exists() {
            remove_dir_forced(dir);
        }
        let _ = std::fs::create_dir_all(dir);
    }
    for is_studio in [false, true] {
        *ctx.distribution_state_mut(is_studio) = DistributionState::default();
        ctx.save_distribution_state(is_studio);
    }
    logger::info("Ruststrap: all Roblox versions and downloads removed");
    Ok(())
}

/// Removes version directories that neither distribution references anymore.
fn cleanup_versions(paths: &Paths, ctx: &Ctx, current_guid: &str) {
    let keep: Vec<&str> = if cfg!(windows) {
        vec![
            current_guid,
            "WindowsPlayer",
            "WindowsStudio64",
            SOBER_DIR_NAME,
        ]
    } else {
        vec![current_guid, "WindowsStudio64", SOBER_DIR_NAME]
    };
    let Ok(entries) = std::fs::read_dir(&paths.versions) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if keep.iter().any(|k| k.eq_ignore_ascii_case(&name)) {
            continue;
        }
        let referenced =
            ctx.player_state.version_guid == name || ctx.studio_state.version_guid == name;
        if referenced || !entry.path().is_dir() {
            continue;
        }
        logger::info(&format!("Deleting old version directory {name}"));
        remove_dir_forced(&entry.path());
    }
}

/// Deletes cached packages no longer referenced by either distribution state.
fn cleanup_downloads(paths: &Paths, ctx: &Ctx) {
    let keep = ctx.all_cached_package_hashes();
    let Ok(entries) = std::fs::read_dir(&paths.downloads) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if keep.iter().any(|k| k.eq_ignore_ascii_case(&name)) {
            continue;
        }
        logger::info(&format!("Deleting unused package {name}"));
        if let Err(e) = std::fs::remove_file(&path) {
            logger::error(&format!("Failed to delete {name}: {e}"));
        }
    }
}

/// Lays user modifications and fast flags over a version directory.
pub fn apply_modifications(paths: &Paths, version_dir: &Path) {
    if !paths.modifications.exists() {
        let _ = std::fs::create_dir_all(&paths.modifications);
    }
    match copy_dir_overwrite(&paths.modifications, version_dir) {
        Ok(()) => logger::info("Modifications applied"),
        Err(e) => logger::error(&format!("Failed to apply modifications: {e}")),
    }
    fastflags::apply_to_version_dir(paths, version_dir);
}
