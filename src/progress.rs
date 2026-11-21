// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

use std::sync::Mutex;
use std::sync::OnceLock;

static CURRENT: OnceLock<Mutex<Option<Progress>>> = OnceLock::new();

#[derive(Clone, Debug, PartialEq)]
pub struct Progress {
    pub stage: String,
    pub done: u64,
    pub total: u64,
}

/// Indeterminate stage marker (`done`/`total` ignored by the UI when total is 0).
pub fn set(stage: &str) {
    set_counted(stage, 0, 0);
}

pub fn set_counted(stage: &str, done: u64, total: u64) {
    let mutex = CURRENT.get_or_init(|| Mutex::new(None));
    if let Ok(mut slot) = mutex.lock() {
        *slot = Some(Progress {
            stage: stage.to_string(),
            done,
            total,
        });
    }
}

pub fn clear() {
    if let Some(mutex) = CURRENT.get() {
        if let Ok(mut slot) = mutex.lock() {
            *slot = None;
        }
    }
}

pub fn snapshot() -> Option<Progress> {
    CURRENT.get()?.lock().ok()?.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_clear() {
        set_counted("Downloading", 2, 9);
        let snap = snapshot().unwrap();
        assert_eq!(snap.stage, "Downloading");
        assert_eq!((snap.done, snap.total), (2, 9));
        clear();
        assert_eq!(snapshot(), None);
    }

    #[test]
    fn set_overwrites() {
        set("stage-a");
        set("stage-b");
        assert_eq!(snapshot().unwrap().stage, "stage-b");
        clear();
    }
}
