use crate::{
    bili, collection,
    model::{ArchiveDoc, Video},
    secure,
};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};

pub fn data_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("BILI_COMMENT_HUB_CONFIG_DIR") {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            bail!("BILI_COMMENT_HUB_CONFIG_DIR 必须为绝对路径");
        }
        return Ok(path);
    }
    dirs::config_dir()
        .map(|p| p.join("BiliCommentHub"))
        .ok_or_else(|| anyhow::anyhow!("找不到应用配置目录"))
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Cursor {
    pub offset: String,
    pub main_done: bool,
    pub replies: BTreeMap<u64, u64>, // next page; zero means completed
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Stage {
    pub ty: u32,
    pub page: u64,
    pub done: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Task {
    pub version: u32,
    pub uid: u64,
    pub mode: String,
    pub ranges: collection::Ranges,
    pub page_size: u64,
    pub include_replies: bool,
    pub stages: Vec<Stage>,
    pub rows: Vec<bili::CommentRow>,
    pub targets: Vec<Video>,
    pub cursors: BTreeMap<String, Cursor>,
    pub done: Vec<String>,
}

pub fn key(v: &Video) -> String {
    format!("{}-{}", v.reply_type(), v.aid)
}

impl Task {
    pub fn new(uid: u64, ranges: collection::Ranges, replies: bool, targets: Vec<Video>) -> Self {
        Self {
            version: 2,
            uid,
            mode: if targets.is_empty() {
                "all"
            } else {
                "selected"
            }
            .into(),
            ranges: ranges.clone(),
            page_size: collection::PAGE_SIZE,
            include_replies: replies,
            stages: collection::TYPES
                .into_iter()
                .filter_map(|ty| {
                    ranges.get(&ty).filter(|r| r.enabled).map(|r| Stage {
                        ty,
                        page: r.from,
                        done: false,
                    })
                })
                .collect(),
            rows: vec![],
            targets,
            cursors: BTreeMap::new(),
            done: vec![],
        }
    }
    pub fn id(&self) -> String {
        let mut ids: Vec<_> = self.targets.iter().map(key).collect();
        ids.sort();
        use md5::{Digest, Md5};
        format!(
            "{:x}",
            Md5::digest(format!(
                "{}:{}:{}:{}:{:?}:{}:{:?}",
                self.version,
                self.uid,
                self.mode,
                self.page_size,
                self.ranges,
                self.include_replies,
                ids
            ))
        )
    }
    pub fn path_in(&self, root: &Path) -> PathBuf {
        root.join(self.uid.to_string())
            .join(format!("{}.json", self.id()))
    }
    pub fn path(&self) -> Result<PathBuf> {
        Ok(self.path_in(&data_dir()?.join("checkpoints")))
    }
    pub fn save_to(&self, path: &Path) -> Result<()> {
        secure::write_config(path, &serde_json::to_value(self)?)
    }
    pub fn save(&self) -> Result<()> {
        self.save_to(&self.path()?)
    }
    pub fn load(path: &Path) -> Result<Self> {
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
        if value["version"].as_u64() != Some(2) {
            bail!("旧版 50 条一批的断点不兼容，原文件已保留");
        }
        let task: Self = serde_json::from_value(value)?;
        collection::validate(&task.ranges, false)?;
        if task.version != 2 || task.uid == 0 || task.page_size != collection::PAGE_SIZE {
            bail!("不支持的采集进度");
        }
        Ok(task)
    }
    pub fn resume(self) -> Result<Self> {
        let path = self.path()?;
        if path.exists() {
            let old = Self::load(&path)?;
            if old.id() != self.id() {
                bail!("采集进度与任务不匹配");
            }
            Ok(old)
        } else {
            Ok(self)
        }
    }
    pub fn remove(&self) -> Result<()> {
        remove(&self.path()?)
    }
    pub fn accept(&mut self, ty: u32, page: u64, rows: Vec<bili::CommentRow>, total: u64) {
        let n = rows.len();
        let mut seen: HashSet<_> = self.rows.iter().map(|r| (r.ty, r.oid, r.rpid)).collect();
        for row in rows {
            if seen.insert((row.ty, row.oid, row.rpid)) {
                self.rows.push(row);
            }
        }
        let end = self
            .ranges
            .get(&ty)
            .is_some_and(|r| r.to > 0 && page >= r.to);
        if let Some(s) = self.stages.iter_mut().find(|s| s.ty == ty) {
            s.done = n == 0
                || (total == 0 && (n as u64) < self.page_size)
                || end
                || (total > 0 && page >= total.div_ceil(self.page_size));
            s.page = page + 1;
        }
    }
    pub fn complete(&self) -> bool {
        if self.mode == "all" {
            self.stages.iter().all(|s| s.done)
        } else {
            self.targets.iter().all(|v| self.done.contains(&key(v)))
        }
    }
    pub fn doc(&self) -> ArchiveDoc {
        let mut doc = ArchiveDoc::default();
        let videos = if self.mode == "all" {
            bili::rows_to_videos(&self.rows, self.include_replies, self.uid, "")
        } else {
            self.targets.clone()
        };
        for v in videos {
            doc.merge_video(v);
        }
        doc
    }
    pub fn summary(&self) -> String {
        if self.mode == "selected" {
            return format!(
                "已保存 {}/{} 个稿件进度，可继续采集",
                self.done.len(),
                self.targets.len()
            );
        }
        self.stages
            .iter()
            .filter(|s| !s.done)
            .map(|s| format!("{}第 {} 页", label(s.ty), s.page))
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

pub fn label(ty: u32) -> &'static str {
    match ty {
        11 => "动态",
        17 => "转发动态",
        12 => "专栏",
        14 => "音频",
        _ => "视频",
    }
}
pub fn remove(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
pub struct Discovery {
    pub task: Option<Task>,
    pub notices: Vec<String>,
}
pub fn latest(uid: u64, ranges: &collection::Ranges, replies: bool) -> Result<Discovery> {
    latest_in(&data_dir()?.join("checkpoints"), uid, ranges, replies)
}
pub fn latest_in(
    root: &Path,
    uid: u64,
    ranges: &collection::Ranges,
    replies: bool,
) -> Result<Discovery> {
    let dir = root.join(uid.to_string());
    let mut result = Discovery {
        task: None,
        notices: vec![],
    };
    if !dir.exists() {
        return Ok(result);
    }
    let mut files = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|e| e == "json"))
        .collect::<Vec<_>>();
    files.sort_by_key(|e| std::cmp::Reverse(e.metadata().and_then(|m| m.modified()).ok()));
    for file in files {
        match Task::load(&file.path()) {
            Ok(task)
                if task.uid == uid && task.ranges == *ranges && task.include_replies == replies =>
            {
                if result.task.is_none() {
                    result.task = Some(task);
                }
            }
            Ok(_) => (),
            Err(e) => result.notices.push(format!(
                "未恢复断点 {}：{}",
                file.file_name().to_string_lossy(),
                e
            )),
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mixed_types_restart_and_failure_page() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Task::new(123, collection::defaults(), true, vec![]);
        for ty in [1, 11, 17, 12, 14] {
            t.accept(
                ty,
                1,
                vec![bili::CommentRow {
                    ty,
                    oid: 99,
                    rpid: ty as u64,
                    ..Default::default()
                }],
                100,
            );
            let path = t.path_in(dir.path());
            t.save_to(&path).unwrap();
            t = Task::load(&path).unwrap();
            assert_eq!(t.stages.iter().find(|s| s.ty == ty).unwrap().page, 2);
        }
        assert_eq!(t.rows.len(), 5);
        assert_eq!(
            t.doc().videos.len(),
            5,
            "different types with the same object ID must stay separate"
        );
        assert!(!t.complete());
        // A request failure makes no accept call and must not advance a cursor.
        assert!(t.stages.iter().all(|s| s.page == 2));
        for ty in [1, 11, 17, 12, 14] {
            t.accept(ty, 2, vec![], 100);
        }
        assert!(t.complete());
    }
    #[test]
    fn tasks_are_isolated_and_selected_cursor_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let video = Video {
            aid: 99,
            comment_type: 17,
            ..Default::default()
        };
        let mut t = Task::new(123, collection::defaults(), true, vec![video.clone()]);
        let path = t.path_in(dir.path());
        let mut c = Cursor {
            offset: "next".into(),
            main_done: true,
            ..Default::default()
        };
        c.replies.insert(42, 3);
        t.cursors.insert(key(&video), c);
        t.save_to(&path).unwrap();
        let saved = Task::load(&path).unwrap();
        assert_eq!(saved.cursors[&key(&video)].replies[&42], 3);
        assert_eq!(saved.cursors[&key(&video)].offset, "next");
        let other = Task::new(456, collection::defaults(), true, vec![video]);
        assert_ne!(path, other.path_in(dir.path()));
        let mut changed = t.clone();
        changed.ranges.get_mut(&11).unwrap().from = 2;
        assert_ne!(t.id(), changed.id());
        changed = t.clone();
        changed.include_replies = false;
        assert_ne!(t.id(), changed.id());
        remove(&path).unwrap();
        assert!(!path.exists());
    }
}

#[cfg(test)]
mod independent_range_tests {
    use super::*;
    #[test]
    fn each_type_starts_and_stops_at_its_exact_page() {
        let mut ranges = collection::defaults();
        for (i, ty) in collection::TYPES.into_iter().enumerate() {
            let r = ranges.get_mut(&ty).unwrap();
            r.from = i as u64 + 2;
            r.to = r.from;
        }
        let mut task = Task::new(123, ranges.clone(), true, vec![]);
        for stage in task.stages.clone() {
            assert_eq!(stage.page, ranges[&stage.ty].from);
            task.accept(
                stage.ty,
                stage.page,
                vec![bili::CommentRow {
                    ty: stage.ty,
                    rpid: stage.ty as u64,
                    oid: 1,
                    ..Default::default()
                }],
                1000,
            );
            assert!(task.stages.iter().find(|s| s.ty == stage.ty).unwrap().done);
        }
        assert!(task.complete());
        assert_eq!(task.page_size, 10);
        let mut unbounded = Task::new(123, collection::defaults(), true, vec![]);
        unbounded.accept(
            17,
            1,
            vec![bili::CommentRow {
                ty: 17,
                rpid: 1,
                ..Default::default()
            }],
            11,
        );
        assert!(!unbounded.stages.iter().find(|s| s.ty == 17).unwrap().done);
        unbounded.accept(
            17,
            2,
            vec![bili::CommentRow {
                ty: 17,
                rpid: 2,
                ..Default::default()
            }],
            11,
        );
        assert!(unbounded.stages.iter().find(|s| s.ty == 17).unwrap().done);
    }
    #[test]
    fn discovery_keeps_legacy_files_and_can_restore_v2() {
        let dir = tempfile::tempdir().unwrap();
        let ranges = collection::defaults();
        let task = Task::new(123, ranges.clone(), true, vec![]);
        let path = task.path_in(dir.path());
        task.save_to(&path).unwrap();
        let old = path.parent().unwrap().join("legacy.json");
        std::fs::write(&old, b"{\"version\":1}").unwrap();
        let found = latest_in(dir.path(), 123, &ranges, true).unwrap();
        assert!(found.task.is_some());
        assert_eq!(found.notices.len(), 1);
        assert!(old.exists());
        assert!(latest_in(dir.path(), 456, &ranges, true)
            .unwrap()
            .task
            .is_none());
        let mut changed = ranges;
        changed.get_mut(&14).unwrap().from = 7;
        assert!(latest_in(dir.path(), 123, &changed, true)
            .unwrap()
            .task
            .is_none());
    }
}


#[cfg(test)]
mod page_boundary_tests {
    use super::*;

    fn rows(ty: u32, page: u64, count: usize) -> Vec<bili::CommentRow> {
        (0..count).map(|i| bili::CommentRow {
            ty,
            oid: 42,
            rpid: page * 100 + i as u64,
            ..Default::default()
        }).collect()
    }

    #[test]
    fn unknown_total_stops_only_on_empty_or_short_page() {
        for ty in collection::TYPES {
            for (count, expected_done) in [(0, true), (9, true), (10, false)] {
                let mut task = Task::new(123, collection::defaults(), true, vec![]);
                task.accept(ty, 1, rows(ty, 1, count), 0);
                let stage = task.stages.iter().find(|s| s.ty == ty).unwrap();
                assert_eq!(stage.done, expected_done, "type {ty}, count {count}");
                assert_eq!(stage.page, 2);
            }
        }
    }

    #[test]
    fn known_total_stops_at_its_last_page() {
        for ty in collection::TYPES {
            let mut task = Task::new(123, collection::defaults(), true, vec![]);
            task.accept(ty, 1, rows(ty, 1, 10), 11);
            assert!(!task.stages.iter().find(|s| s.ty == ty).unwrap().done);
            task.accept(ty, 2, rows(ty, 2, 1), 11);
            assert!(task.stages.iter().find(|s| s.ty == ty).unwrap().done);
        }
    }

    #[test]
    fn configured_end_page_stops_even_when_more_comments_exist() {
        for ty in collection::TYPES {
            let mut ranges = collection::defaults();
            let range = ranges.get_mut(&ty).unwrap();
            range.from = 2;
            range.to = 3;
            let mut task = Task::new(123, ranges, true, vec![]);
            task.accept(ty, 2, rows(ty, 2, 10), 100);
            assert!(!task.stages.iter().find(|s| s.ty == ty).unwrap().done);
            task.accept(ty, 3, rows(ty, 3, 10), 100);
            assert!(task.stages.iter().find(|s| s.ty == ty).unwrap().done);
        }
    }
}
