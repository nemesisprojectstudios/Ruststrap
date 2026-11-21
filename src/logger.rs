// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use std::sync::OnceLock;

static LOG_FILE: OnceLock<Mutex<Option<File>>> = OnceLock::new();
static RING: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();
const RING_CAP: usize = 400;

pub fn init(path: PathBuf) {
    let file = OpenOptions::new().create(true).append(true).open(path).ok();
    let _ = LOG_FILE.set(Mutex::new(file));
    let _ = RING.set(Mutex::new(VecDeque::with_capacity(RING_CAP)));
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
    for line in msg.lines() {
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
    RING
        .get()
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
}
