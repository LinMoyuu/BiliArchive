use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{path::{Path, PathBuf}, sync::Mutex};

pub const FILE_NAME: &str = "collection-time.json";
const FORMAT: &str = "bili-collection-time";
static WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Serialize, Deserialize)]
pub struct Stamp {
    format: String,
    pub uid: u64,
    pub saved_at_ms: i64,
}

pub fn path(archive_root: &Path, uid: u64) -> PathBuf {
    archive_root.join(uid.to_string()).join(FILE_NAME)
}

impl Stamp {
    pub fn parse(value: serde_json::Value) -> Result<Self> {
        let stamp: Self = serde_json::from_value(value).context("采集时间记录格式错误")?;
        anyhow::ensure!(
            stamp.format == FORMAT && stamp.uid != 0 && stamp.saved_at_ms > 0
                && chrono::DateTime::from_timestamp_millis(stamp.saved_at_ms).is_some(),
            "采集时间记录无效"
        );
        Ok(stamp)
    }

    pub fn merge(&self, archive_root: &Path) -> Result<()> {
        let _guard = WRITE_LOCK.lock().unwrap();
        if read(archive_root, self.uid).is_some_and(|old| old >= self.saved_at_ms) {
            return Ok(());
        }
        crate::secure::write_config(&path(archive_root, self.uid), &serde_json::to_value(self)?)
    }
}

pub fn read(archive_root: &Path, uid: u64) -> Option<i64> {
    if uid == 0 || archive_root.as_os_str().is_empty() {
        return None;
    }
    let value = serde_json::from_slice(&std::fs::read(path(archive_root, uid)).ok()?).ok()?;
    let stamp = Stamp::parse(value).ok()?;
    (stamp.uid == uid).then_some(stamp.saved_at_ms)
}

pub fn record(archive_root: &Path, uid: u64) -> Result<()> {
    if uid == 0 {
        return Ok(());
    }
    Stamp {
        format: FORMAT.into(),
        uid,
        saved_at_ms: chrono::Utc::now().timestamp_millis(),
    }.merge(archive_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_portable_times_without_replacing_a_newer_record() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let mut stamp = Stamp { format: FORMAT.into(), uid: 12, saved_at_ms: 1000 };
        stamp.merge(source.path()).unwrap();
        let stored = std::fs::read(path(source.path(), 12)).unwrap();
        Stamp::parse(serde_json::from_slice(&stored).unwrap()).unwrap().merge(target.path()).unwrap();
        assert_eq!(read(target.path(), 12), Some(1000));
        stamp.saved_at_ms = 2000;
        stamp.merge(target.path()).unwrap();
        Stamp::parse(serde_json::from_slice(&stored).unwrap()).unwrap().merge(target.path()).unwrap();
        assert_eq!(read(target.path(), 12), Some(2000));
        assert_eq!(read(target.path(), 13), None);
        assert_eq!(read(source.path(), 12), Some(1000));
    }
}
