// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::logger;

/// A very small complementary process lock implemented with O_EXCL file
/// creation. No cross-platform libraries needed; the lock records its holder's
/// PID so a lock left behind by a crashed or killed process is reclaimed
/// immediately instead of blocking for `STALE_AFTER`.
pub struct FileLock {
    path: Option<PathBuf>,
}

const STALE_AFTER: Duration = Duration::from_secs(30 * 60);
const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// How often a contended lock repeats that it is still waiting.
const WAIT_LOG_EVERY: Duration = Duration::from_secs(30);

impl FileLock {
    /// Returns None while another process holds the lock with the same name.
    pub fn try_acquire(dir: &Path, name: &str) -> Option<FileLock> {
        let path = dir.join(format!("{name}.lock"));
        match OpenOptions::new().create_new(true).write(true).open(&path) {
            Ok(mut file) => {
                let _ = writeln!(file, "{}", std::process::id());
                Some(FileLock { path: Some(path) })
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => None,
            Err(err) => {
                logger::error(&format!("Failed to create lock {name}: {err}"));
                // proceed unlocked rather than wedging the bootstrapper
                Some(FileLock { path: None })
            }
        }
    }

    /// Blocks until the lock is free (reclaiming stale locks), polling.
    pub fn acquire(dir: &Path, name: &str) -> FileLock {
        let path = dir.join(format!("{name}.lock"));
        // subtracting the interval makes the first contention log immediately
        let mut last_wait_log = Instant::now() - WAIT_LOG_EVERY;
        loop {
            if let Some(lock) = FileLock::try_acquire(dir, name) {
                return lock;
            }
            if is_stale(&path) {
                logger::warn(&format!(
                    "Reclaiming {name} lock: its holder no longer exists"
                ));
                let _ = std::fs::remove_file(&path);
                continue;
            }
            if last_wait_log.elapsed() >= WAIT_LOG_EVERY {
                logger::info(&format!("{name} instance exists, waiting..."));
                last_wait_log = Instant::now();
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }
}

/// A lock is stale when its recorded holder no longer runs, or when the file
/// is older than `STALE_AFTER` (covers unreadable holders and PID reuse).
fn is_stale(path: &Path) -> bool {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(_) => return false,
    };
    let aged = meta
        .modified()
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age > STALE_AFTER);
    match std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok())
    {
        Some(pid) => !pid_alive(pid) || aged,
        None => aged,
    }
}

/// Tells whether a PID is running, without extra dependencies: /proc on Linux,
/// tasklist on Windows. Unknown is always treated as alive.
fn pid_alive(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    if cfg!(windows) {
        let output = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output();
        match output {
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout);
                let pid_text = pid.to_string();
                text.split(',')
                    .any(|field| field.trim_matches('"') == pid_text)
            }
            Err(_) => true,
        }
    } else {
        Path::new(&format!("/proc/{pid}")).exists()
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ruststrap-lock-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn dead_holder_is_stale_even_when_fresh() {
        let dir = temp_dir("stale");
        let path = dir.join("Test.lock");
        std::fs::write(&path, "999999999\n").unwrap();
        assert!(is_stale(&path), "a lock whose PID is gone must be stale");
        std::fs::write(&path, format!("{}\n", std::process::id())).unwrap();
        assert!(!is_stale(&path), "a lock held by this process is fresh");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unparsable_lock_waits_for_the_age_limit() {
        let dir = temp_dir("junk");
        let path = dir.join("Test.lock");
        std::fs::write(&path, "not a pid").unwrap();
        assert!(!is_stale(&path));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
