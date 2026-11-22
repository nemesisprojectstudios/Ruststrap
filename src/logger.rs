// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use std::sync::OnceLock;

static LOG_FILE: OnceLock<Mutex<Option<File>>> = OnceLock::new();
static RING: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
/// The last (level, line) written; identical consecutive entries are dropped
/// so polling wait loops cannot flood the log.
static LAST_LINE: OnceLock<Mutex<Option<(String, String)>>> = OnceLock::new();
const RING_CAP: usize = 400;

/// Sessions older than this get a fresh log file instead of appending.
const ROTATE_AFTER: Duration = Duration::from_secs(6 * 3600);
/// Log files older than this are deleted at startup.
const PRUNE_AFTER: Duration = Duration::from_secs(7 * 86400);
/// `latest.log` points at the file the current session writes to.
const LATEST_NAME: &str = "latest.log";

/// Rotates, prunes, opens the log file, and points `latest.log` at it.
pub fn init(dir: PathBuf) {
    let _ = std::fs::create_dir_all(&dir);
    let now = SystemTime::now();
    prune_old_logs(&dir, now);
    let path = rotate_target(&dir, now);
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok();
    let _ = LOG_FILE.set(Mutex::new(file));
    let _ = RING.set(Mutex::new(VecDeque::with_capacity(RING_CAP)));
    let _ = LOG_PATH.set(path.clone());
    write_latest_link(&dir, &path);
}

/// The file the current session logs into, if any.
pub fn file_path() -> Option<PathBuf> {
    LOG_PATH.get().cloned()
}

/// The newest existing session log if it was written to within
/// `ROTATE_AFTER`, otherwise a fresh timestamped file name.
fn rotate_target(dir: &Path, now: SystemTime) -> PathBuf {
    if let Some((newest, mtime)) = newest_log(dir) {
        if now
            .duration_since(mtime)
            .map(|age| age <= ROTATE_AFTER)
            .unwrap_or(false)
        {
            return newest;
        }
    }
    let secs = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    let (y, mo, d, h, mi, s) = civil_from_secs(secs);
    let mut path = dir.join(format!(
        "Ruststrap-{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}.log"
    ));
    let mut suffix = 1u32;
    while path.exists() {
        path = dir.join(format!(
            "Ruststrap-{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}-{suffix}.log"
        ));
        suffix += 1;
    }
    path
}

/// Newest session log (legacy `Ruststrap.log` or `Ruststrap-*.log`) by mtime.
fn newest_log(dir: &Path) -> Option<(PathBuf, SystemTime)> {
    let mut best: Option<(PathBuf, SystemTime)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_log =
            name == "Ruststrap.log" || (name.starts_with("Ruststrap-") && name.ends_with(".log"));
        if !is_log || !entry.path().is_file() {
            continue;
        }
        let mtime = entry.metadata().and_then(|m| m.modified()).ok()?;
        if best.as_ref().is_none_or(|(_, t)| mtime > *t) {
            best = Some((entry.path(), mtime));
        }
    }
    best
}

/// Deletes session logs (and a stale on-disk `latest.log`) older than `PRUNE_AFTER`.
fn prune_old_logs(dir: &Path, now: SystemTime) {
    let cutoff = now.checked_sub(PRUNE_AFTER);
    let Some(cutoff) = cutoff else { return };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_log = name == LATEST_NAME
            || name == "Ruststrap.log"
            || (name.starts_with("Ruststrap-") && name.ends_with(".log"));
        if !is_log {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|mtime| mtime < cutoff)
            .unwrap_or(false);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Points `latest.log` at the active log (symlink on Unix, best-effort copy on
/// Windows where symlinks need elevated or developer-mode rights).
fn write_latest_link(dir: &Path, path: &Path) {
    let latest = dir.join(LATEST_NAME);
    let _ = std::fs::remove_file(&latest);
    #[cfg(unix)]
    {
        if std::os::unix::fs::symlink(&path, &latest).is_err() {
            let _ = std::fs::copy(&path, &latest);
        }
    }
    #[cfg(windows)]
    {
        if std::os::windows::fs::symlink_file(&path, &latest).is_err() {
            let _ = std::fs::copy(&path, &latest);
        }
    }
}

pub fn info(msg: &str) {
    log("INFO", msg);
}

pub fn warn(msg: &str) {
    log("WARN", msg);
}

pub fn error(msg: &str) {
    log("ERROR", msg);
}

pub fn log(level: &str, msg: &str) {
    let mut last = LAST_LINE.get_or_init(|| Mutex::new(None)).lock().ok();
    for line in msg.lines() {
        // a repeat of the previous entry (timestamps excluded) is suppressed
        if last
            .as_deref()
            .is_some_and(|prev| prev.as_ref() == Some(&(level.to_string(), line.to_string())))
        {
            continue;
        }
        if let Some(prev) = last.as_deref_mut() {
            *prev = Some((level.to_string(), line.to_string()));
        }
        let out = format!("[{}] [{}] {}", timestamp(), level, line);
        let _ = writeln!(std::io::stderr(), "{}", out);
        if let Some(mut guard) = LOG_FILE.get().and_then(|m| m.lock().ok()) {
            if let Some(file) = guard.as_mut() {
                let _ = writeln!(file, "{}", out);
            }
        }
        if let Some(mut ring) = RING.get().and_then(|m| m.lock().ok()) {
            if ring.len() == RING_CAP {
                ring.pop_front();
            }
            ring.push_back(out);
        }
    }
}

/// Snapshot of the log ring buffer for the UI.
pub fn recent() -> Vec<String> {
    RING.get()
        .and_then(|m| m.lock().ok())
        .map(|ring| ring.iter().cloned().collect())
        .unwrap_or_default()
}

fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    let (y, mo, d, h, mi, s) = civil_from_secs(secs);
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, mo, d, h, mi, s)
}

/// Days since 1970-01-01 -> (year, month, day); Howard Hinnant's civil algorithm.
fn civil_from_secs(secs: u64) -> (u64, u32, u32, u32, u32, u32) {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y } as u64;
    (
        y,
        m,
        d,
        (rem / 3600) as u32,
        (rem % 3600 / 60) as u32,
        (rem % 60) as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_formats() {
        assert_eq!(civil_from_secs(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(civil_from_secs(951782400), (2000, 2, 29, 0, 0, 0));
        assert_eq!(civil_from_secs(4102444799), (2099, 12, 31, 23, 59, 59));
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ruststrap-logger-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn rotation_reuses_recent_log_and_creates_timestamped_file_when_stale() {
        let dir = temp_dir("rotate");
        let recent = dir.join("Ruststrap-20260101-000000.log");
        std::fs::write(&recent, "x").unwrap();
        let mtime = recent.metadata().unwrap().modified().unwrap();
        assert_eq!(rotate_target(&dir, mtime), recent);

        let stale_now = mtime + ROTATE_AFTER + Duration::from_secs(1);
        let fresh = rotate_target(&dir, stale_now);
        assert_ne!(fresh, recent);
        assert!(fresh.to_string_lossy().contains("Ruststrap-"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn prune_respects_the_age_limit_both_ways() {
        let dir = temp_dir("prune");
        let fresh = dir.join("Ruststrap-20990101-000000.log");
        let untracked = dir.join("keep-me.txt");
        std::fs::write(&fresh, "log").unwrap();
        std::fs::write(&untracked, "keep").unwrap();

        let now = SystemTime::now();
        prune_old_logs(&dir, now);
        assert!(fresh.exists(), "nothing may be pruned before the age limit");

        let far_future = now + PRUNE_AFTER + Duration::from_secs(2);
        prune_old_logs(&dir, far_future);
        assert!(
            !fresh.exists(),
            "session logs past the age limit are pruned"
        );
        assert!(
            untracked.exists(),
            "files outside the log pattern must survive"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Logger state is process-global (OnceLock); every test that needs it
    /// initialized shares one directory and a mutex so parallel tests agree
    /// (same pattern as the progress tests' TEST_LOCK).
    fn shared_log_dir() -> (PathBuf, std::sync::MutexGuard<'static, ()>) {
        static DIR: OnceLock<PathBuf> = OnceLock::new();
        static LOCK: Mutex<()> = Mutex::new(());
        let guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = DIR.get_or_init(|| temp_dir("shared")).clone();
        logger_init_for_test(dir.clone());
        (dir, guard)
    }

    #[test]
    fn init_writes_latest_link_at_the_active_file() {
        let (dir, _guard) = shared_log_dir();
        let active = file_path().unwrap();
        assert!(active.is_file());
        let latest = dir.join(LATEST_NAME);
        let link_target = std::fs::read_link(&latest);
        match link_target {
            Ok(target) => assert_eq!(target, active, "unix symlink must point at the active log"),
            Err(_) => {
                assert!(latest.is_file(), "copy fallback must produce latest.log");
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn logger_init_for_test(dir: PathBuf) {
        init(dir);
    }

    #[test]
    fn consecutive_duplicate_lines_are_written_once() {
        let (_dir, _guard) = shared_log_dir();
        let before = recent().len();
        log("INFO", "dedupe-probe");
        log("INFO", "dedupe-probe");
        log("INFO", "dedupe-probe");
        assert_eq!(recent().len(), before + 1);
        log("INFO", "dedupe-probe-2");
        assert_eq!(recent().len(), before + 2);
        log("INFO", "dedupe-probe");
        assert_eq!(recent().len(), before + 3);
    }
}
