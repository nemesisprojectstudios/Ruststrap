// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::logger;
use crate::paths::Paths;

/// Client-log markers that indicate the session ended with a disconnect
/// rather than a normal quit (Froststrap's ActivityWatcher watches the same
/// reason codes; these are the textual markers the client writes).
const DISCONNECT_MARKERS: [&str; 4] = [
    "kicked",
    "inactivity",
    "lostconnection",
    "connectionerror",
];

/// Froststrap rejoins after 3 seconds and gives up after a few attempts.
pub const REJOIN_DELAY: Duration = Duration::from_secs(3);
const MAX_REJOINS: u32 = 3;

/// How far back into each new log file to scan for markers.
const TAIL_BYTES: u64 = 96 * 1024;

/// Directories where the active client writes its per-session logs.
pub fn client_log_dirs(paths: &Paths) -> Vec<PathBuf> {
    if cfg!(windows) {
        vec![paths.roblox.join("logs")]
    } else {
        vec![crate::sober::data_dir(paths).join("data/sober/sober_logs")]
    }
}

/// True when any client log written after `since` contains a disconnect
/// marker. Only the newest few files are sampled, reading just their tail.
pub fn detected_since(dirs: &[PathBuf], since: SystemTime) -> bool {
    let mut candidates: Vec<(SystemTime, PathBuf)> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
                if modified >= since {
                    candidates.push((modified, path));
                }
            }
        }
    }
    candidates.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    for (_, path) in candidates.iter().take(5) {
        if file_tail_contains(path, &DISCONNECT_MARKERS) {
            return true;
        }
    }
    false
}

fn file_tail_contains(path: &Path, markers: &[&str]) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let len = match file.metadata() {
        Ok(metadata) => metadata.len(),
        Err(_) => return false,
    };
    let start = len.saturating_sub(TAIL_BYTES);
    if std::io::Seek::seek(&mut file, SeekFrom::Start(start)).is_err() {
        return false;
    }
    let mut buffer = Vec::new();
    if std::io::Read::read_to_end(&mut file, &mut buffer).is_err() {
        return false;
    }
    let haystack = String::from_utf8_lossy(&buffer).to_lowercase();
    markers
        .iter()
        .any(|marker| haystack.contains(marker))
}

/// After the client exits, checks its logs for disconnect evidence and
/// rejoins the same place, bounded. Returns how many rejoins happened.
/// `since` is the moment the watched session started; `relaunch` must spawn
/// a fresh client (its exit is waited on here).
pub fn reconnect_loop(
    enabled: bool,
    joined_a_place: bool,
    log_dirs: &[PathBuf],
    process_label: &str,
    since: &mut SystemTime,
    delay: Duration,
    relaunch: &mut dyn FnMut() -> Result<std::process::Child, String>,
) -> u32 {
    if !enabled || !joined_a_place {
        return 0;
    }
    let mut rejoins = 0;
    while rejoins < MAX_REJOINS {
        if !detected_since(log_dirs, *since) {
            break;
        }
        if rejoins == 0 {
            logger::info("Disconnect detected in the client logs; auto-rejoining");
        } else {
            logger::info(&format!("Auto-rejoin {rejoins} also ended in a disconnect; rejoining again"));
        }
        std::thread::sleep(delay);
        match relaunch() {
            Ok(child) => {
                *since = SystemTime::now();
                rejoins += 1;
                crate::watcher::wait_for_exit(child, process_label);
            }
            Err(e) => {
                logger::error(&format!("Auto-rejoin failed to relaunch the client: {e}"));
                break;
            }
        }
    }
    rejoins
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> TempDir {
            let base = std::env::temp_dir().join(format!(
                "ruststrap-test-rejoin-{}-{name}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&base);
            std::fs::create_dir_all(&base).unwrap();
            TempDir(base)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn detection_needs_a_marker_after_the_cutoff() {
        let dir = TempDir::new("detect");
        let dirs = vec![dir.path().to_path_buf()];

        assert!(!detected_since(&dirs, SystemTime::now()), "empty dir");

        write(dir.path(), "session.log", "2:40:00 ConnectionError: kicked\n");
        std::thread::sleep(Duration::from_millis(20));

        let before = SystemTime::now() - Duration::from_secs(1);
        assert!(
            detected_since(&dirs, before),
            "log newer than the cutoff with a marker counts"
        );

        let future = SystemTime::now() + Duration::from_secs(3600);
        assert!(
            !detected_since(&dirs, future),
            "logs older than the cutoff never count"
        );
    }

    #[test]
    fn markerless_new_sessions_do_not_trip_detection() {
        let dir = TempDir::new("clean-session");
        let dirs = vec![dir.path().to_path_buf()];
        write(dir.path(), "session.log", "just joining normally\n");
        std::thread::sleep(Duration::from_millis(20));
        let before = SystemTime::now() - Duration::from_secs(1);
        assert!(!detected_since(&dirs, before));
    }

    #[test]
    fn reconnect_loop_is_bounded_and_honors_the_switch() {
        let dir = TempDir::new("loop");
        let dirs = vec![dir.path().to_path_buf()];
        let mut since = SystemTime::now() - Duration::from_secs(1);
        write(dir.path(), "session.log", "ConnectionError: kicked\n");

        let launches = std::cell::Cell::new(0u32);
        let mut relaunch = || -> Result<std::process::Child, String> {
            launches.set(launches.get() + 1);
            Err("no client here".to_string())
        };

        let zero = Duration::ZERO;
        assert_eq!(
            reconnect_loop(false, true, &dirs, "x", &mut since, zero, &mut relaunch),
            0
        );
        assert_eq!(launches.get(), 0, "disabled means no attempts");
        assert_eq!(
            reconnect_loop(true, false, &dirs, "x", &mut since, zero, &mut relaunch),
            0
        );
        assert_eq!(launches.get(), 0, "no place join means no attempts");

        // relaunching always fails, so the loop stops after one failed attempt
        assert_eq!(
            reconnect_loop(true, true, &dirs, "x", &mut since, zero, &mut relaunch),
            0
        );
        assert_eq!(launches.get(), 1, "one relaunch was attempted before giving up");
    }
}
