//! Portable library identity. Machine paths are locators, never archive identity.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub version: u32,
    pub library_id: String,
}
#[derive(Clone)]
pub struct Active {
    pub id: String,
    pub root: PathBuf,
    pub offline: bool,
}
static ACTIVE: Mutex<Option<Active>> = Mutex::new(None);
static COMMENT_TASK_MIGRATION: Mutex<()> = Mutex::new(());
pub fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}
pub fn offline() -> bool {
    active().is_some_and(|a| a.offline)
}
pub fn active() -> Option<Active> {
    ACTIVE.lock().unwrap().clone()
}
pub fn set_active(value: Option<Active>) {
    *ACTIVE.lock().unwrap() = value;
}
pub fn configured_export_root() -> PathBuf {
    if let Some(a) = active() {
        return a.root;
    }
    let data = crate::checkpoint::data_dir().unwrap_or_default();
    let cfg: Value = std::fs::read(data.join("config.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    cfg["export_dir"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| data.join("exports"))
}
pub fn ensure(root: &Path) -> Result<Manifest> {
    let root = crate::archive::root(root);
    let path = root.join(".library.json");
    if path.exists() {
        let m: Manifest = serde_json::from_slice(&std::fs::read(path)?)?;
        anyhow::ensure!(
            m.format == "bili-comment-hub-library" && m.version == 1 && valid_id(&m.library_id),
            "资料库标识无效，未覆盖"
        );
        return Ok(m);
    }
    std::fs::create_dir_all(&root)?;
    let entropy = tempfile::NamedTempFile::new_in(&root)?;
    let id = crate::archive::hash(&format!(
        "{}:{}:{}",
        entropy.path().display(),
        std::process::id(),
        chrono::Local::now()
            .timestamp_nanos_opt()
            .unwrap_or_default()
    ));
    let m = Manifest {
        format: "bili-comment-hub-library".into(),
        version: 1,
        library_id: id,
    };
    // create_new avoids two readers inventing different identities for the same library.
    use std::io::Write;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut f) => {
            f.write_all(&serde_json::to_vec_pretty(&m)?)?;
            f.sync_all()?;
            Ok(m)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => ensure(&root),
        Err(e) => Err(e.into()),
    }
}
pub fn id_for_archive(path: &Path) -> Result<String> {
    if path
        .ancestors()
        .any(|p| p.file_name().is_some_and(|n| n == "archive-v1"))
    {
        Ok(ensure(&crate::archive::storage_root(path))?.library_id)
    } else {
        // Synthetic worker fixtures are scoped without writing into a real profile.
        Ok(crate::archive::hash(&format!("fixture:{}", path.display())))
    }
}
pub fn current_id() -> Result<String> {
    if let Some(a) = active() {
        Ok(a.id)
    } else {
        Ok(ensure(&configured_export_root())?.library_id)
    }
}
pub fn state_dir(name: &str) -> Result<PathBuf> {
    Ok(crate::checkpoint::data_dir()?
        .join(name)
        .join(current_id()?))
}
pub fn state_matches(value: &Value, uid: u64, root: &Path) -> Result<bool> {
    if value["uid"].as_u64() != Some(uid) {
        return Ok(false);
    }
    if let Some(id) = value["library_id"].as_str() {
        return Ok(id == id_for_archive(root)?);
    }
    Ok(value["archive_root"]
        .as_str()
        .is_some_and(|s| Path::new(s) == root))
}
pub fn read_state(path: &Path, uid: u64, root: &Path) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let mut value: Value =
        serde_json::from_slice(&std::fs::read(path)?).context("任务记录损坏，未覆盖")?;
    if !state_matches(&value, uid, root)? {
        return Ok(None);
    }
    if value.get("archive_root").is_some() {
        value
            .as_object_mut()
            .context("任务记录无效")?
            .remove("archive_root");
        value["library_id"] = json!(id_for_archive(root)?);
    }
    Ok(Some(value))
}
pub fn migrate_state(old: &Path, new: &Path, uid: u64, root: &Path) -> Result<bool> {
    let Some(value) = read_state(old, uid, root)? else {
        return Ok(false);
    };
    if !new.exists() {
        crate::secure::write_config(new, &value)?;
    }
    let saved: Value = serde_json::from_slice(&std::fs::read(new)?)?;
    anyhow::ensure!(
        state_matches(&saved, uid, root)?,
        "迁移目标归属不一致，原记录已保留"
    );
    if old != new {
        backup_remove(old)?;
    }
    Ok(true)
}
pub fn backup_remove(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let bytes = std::fs::read(path)?;
    let name = format!(
        "{}-{}",
        crate::archive::hash(&path.to_string_lossy()),
        path.file_name().unwrap_or_default().to_string_lossy()
    );
    let profile = crate::checkpoint::data_dir()?;
    let base = if path.starts_with(&profile) {
        profile
    } else {
        path.parent().unwrap_or(Path::new(".")).to_path_buf()
    };
    let backup = base.join("migrations/legacy-state").join(name);
    std::fs::create_dir_all(backup.parent().unwrap())?;
    if backup.exists() {
        anyhow::ensure!(
            std::fs::read(&backup)? == bytes,
            "旧记录备份冲突，原文件已保留"
        );
    } else {
        std::fs::write(&backup, &bytes)?;
    }
    anyhow::ensure!(std::fs::read(backup)? == bytes, "旧记录备份校验失败");
    std::fs::remove_file(path)?;
    Ok(())
}
pub fn migrate_message_task(uid: u64, new: &Path) -> Result<()> {
    let old = crate::checkpoint::data_dir()?
        .join("checkpoints/messages/v2")
        .join(format!("{uid}.json"));
    let root = crate::messages::base(&configured_export_root(), uid);
    if old.exists() {
        migrate_state(&old, new, uid, &root)?;
    }
    Ok(())
}
pub fn migrate_comment_tasks(new_root: &Path) -> Result<()> {
    // Imported libraries must not claim the original configured library's old comment tasks.
    if active().is_some() {
        return Ok(());
    }
    let data = crate::checkpoint::data_dir()?;
    let id = current_id()?;
    migrate_comment_tasks_at(&data, new_root, &id)
}

fn migrate_comment_tasks_at(data: &Path, new_root: &Path, id: &str) -> Result<()> {
    // Archive readers can reach this in parallel. Hold the lock through backup and marker creation.
    let _migration = COMMENT_TASK_MIGRATION.lock().unwrap();
    let marker = data.join("migrations/checkpoint-library.json");
    if marker.exists() {
        return Ok(());
    }
    let old = data.join("checkpoints");
    if old.is_dir() {
        for entry in std::fs::read_dir(&old)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !entry.file_type()?.is_dir() || name.parse::<u64>().is_err() {
                continue;
            }
            for file in std::fs::read_dir(entry.path())? {
                let file = file?;
                if !file.file_type()?.is_file()
                    || file.path().extension().is_none_or(|s| s != "json")
                {
                    continue;
                }
                let value: Value = serde_json::from_slice(&std::fs::read(file.path())?)
                    .context("旧评论断点损坏，未迁移")?;
                if value["version"] != 3 {
                    continue;
                }
                if value["uid"].as_u64().map(|u| u.to_string()) != Some(name.clone()) {
                    bail!("旧评论断点账号不匹配，未迁移");
                }
                let dest = new_root.join(&name).join(file.file_name());
                if !dest.exists() {
                    crate::secure::write_config(&dest, &value)?;
                }
                backup_remove(&file.path())?;
            }
        }
    }
    crate::secure::write_config(&marker, &json!({"library_id":id}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn library_identity_survives_directory_moves_and_independent_roots_differ() {
        let t = tempfile::tempdir().unwrap();
        let first = t.path().join("first");
        let a = ensure(&first).unwrap();
        assert_eq!(a.library_id, ensure(&first).unwrap().library_id);
        let moved = t.path().join("moved");
        std::fs::rename(&first, &moved).unwrap();
        assert_eq!(a.library_id, ensure(&moved).unwrap().library_id);
        assert_ne!(
            a.library_id,
            ensure(&t.path().join("other")).unwrap().library_id
        );
    }
    #[test]
    fn legacy_state_is_converted_only_for_the_original_uid_and_library() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("archive-v1/12/messages");
        let id = id_for_archive(&root).unwrap();
        let old = t.path().join("old.json");
        crate::secure::write_config(&old, &json!({"uid":12,"archive_root":root,"version":2}))
            .unwrap();
        assert!(read_state(&old, 13, &root).unwrap().is_none());
        assert!(
            read_state(&old, 12, &t.path().join("other/archive-v1/12/messages"))
                .unwrap()
                .is_none()
        );
        let new = t.path().join("new.json");
        assert!(migrate_state(&old, &new, 12, &root).unwrap());
        assert!(!old.exists());
        let value: Value = serde_json::from_slice(&std::fs::read(new).unwrap()).unwrap();
        assert_eq!(value["library_id"], id);
        assert!(value.get("archive_root").is_none());
        assert!(t.path().join("migrations/legacy-state").is_dir());
    }
    #[test]
    fn concurrent_comment_task_migrations_share_the_completion_marker() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path();
        let root = data.join("checkpoints/library");
        let start = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    start.wait();
                    migrate_comment_tasks_at(data, &root, "library").unwrap();
                });
            }
        });
        let marker = data.join("migrations/checkpoint-library.json");
        let value: Value = serde_json::from_slice(&std::fs::read(marker).unwrap()).unwrap();
        assert_eq!(value, json!({"library_id":"library"}));
    }

    #[test]
    fn corrupt_manifest_is_retained() {
        let t = tempfile::tempdir().unwrap();
        let path = crate::archive::root(t.path()).join(".library.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"broken").unwrap();
        assert!(ensure(t.path()).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"broken");
    }
}
