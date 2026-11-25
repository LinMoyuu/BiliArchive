use crate::model::{ArchiveDoc, Video};
use anyhow::{anyhow, Result};
use std::path::Path;

/// 导出为 JSON（唯一格式）。
/// 评论是主评论挂楼中楼的层级树，JSON 天然贴合；文本里常见的引号 / 反斜杠 / emoji
/// 按 JSON 规则转义即可，消费方（网页、脚本、表格）拿到就能直接读。
pub fn to_json(doc: &ArchiveDoc) -> Result<String> {
    Ok(serde_json::to_string_pretty(
        &crate::archive::from_comments(
            doc,
            crate::archive::comment_account(doc),
            &crate::archive::comment_root(doc),
            crate::archive::ResourcePolicy::load(),
        ),
    )?)
}

pub fn write_file(path: &Path, content: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let mut archive = crate::archive::Envelope::parse(content.as_bytes())?;
    crate::archive::copy_resources(
        &mut archive,
        &crate::archive::configured_root(),
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?;
    crate::archive::write(path, &archive)
}

/// 读取导出的 JSON 文件
pub fn load_doc(path: &Path) -> Result<ArchiveDoc> {
    let text = std::fs::read_to_string(path)?;
    if !text.trim_start().starts_with('{') {
        return Err(anyhow!(
            "这个文件不是 JSON（本工具只导出 JSON，扩展名 .json）"
        ));
    }
    let archive = crate::archive::Envelope::parse(text.as_bytes())?;
    let mut doc = crate::archive::comments(&archive)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    doc.archive["resource_root"] = serde_json::json!(crate::archive::storage_root(parent)
        .canonicalize()
        .unwrap_or_else(|_| parent.to_path_buf()));
    Ok(doc)
}

pub fn category(video: &Video) -> &'static str {
    match video.reply_type() {
        11 => "dynamics",
        17 => "reposts",
        12 => "articles",
        14 => "audio",
        _ => "videos",
    }
}

pub fn item_path(root: &Path, video: &Video) -> Result<std::path::PathBuf> {
    item_path_for(root, video, crate::archive::account())
}
pub fn item_path_for(root: &Path, video: &Video, account: u64) -> Result<std::path::PathBuf> {
    if video.owner_mid == 0 || video.aid == 0 {
        return Err(anyhow!("缺少作者 UID 或稿件 ID，无法确定归档路径"));
    }
    Ok(crate::archive::root(root)
        .join(if account == 0 {
            "public".into()
        } else {
            account.to_string()
        })
        .join("comments")
        .join(video.owner_mid.to_string())
        .join(category(video))
        .join(format!("{}.json", video.aid)))
}

/// Persist content before advancing its checkpoint. Replaying a partially written batch is safe.
pub fn persist_comments(
    root: &Path,
    account: u64,
    videos: &[Video],
    checkpoint: impl FnOnce() -> Result<()>,
) -> Result<usize> {
    for video in videos {
        export_item_for(root, video, account)?;
    }
    checkpoint()?;
    Ok(videos.len())
}

/// One stable file per author/type/object. Never overwrite an unreadable archive.
pub fn export_item(root: &Path, video: &Video) -> Result<std::path::PathBuf> {
    export_item_for(root, video, crate::archive::account())
}
pub fn export_item_for(root: &Path, video: &Video, account: u64) -> Result<std::path::PathBuf> {
    let path = item_path_for(root, video, account)?;
    let mut doc = if path.exists() {
        let old = load_doc(&path)?;
        let expected = if account == 0 {
            serde_json::Value::Null
        } else {
            serde_json::json!(account.to_string())
        };
        if old.archive["meta"]["account_uid"] != expected {
            return Err(anyhow!("原归档账号不匹配，未覆盖"));
        }
        if old.videos.len() != 1
            || old.videos[0].aid != video.aid
            || old.videos[0].reply_type() != video.reply_type()
            || old.videos[0].owner_mid != video.owner_mid
        {
            return Err(anyhow!("原文件与稿件不匹配，已保留"));
        }
        old
    } else {
        ArchiveDoc::default()
    };
    doc.merge_video(video.clone());
    doc.meta.exported_at = crate::model::now_text();
    crate::archive::write(
        &path,
        &crate::archive::from_comments(
            &doc,
            account,
            &crate::archive::root(root),
            crate::archive::ResourcePolicy::load(),
        ),
    )?;
    Ok(path)
}

#[cfg(test)]
mod item_export_tests {
    use super::*;
    use crate::model::Comment;
    #[test]
    fn stable_paths_and_incremental_single_item() {
        let root = tempfile::tempdir().unwrap();
        let mut video = Video {
            aid: 42,
            owner_mid: 123,
            comment_type: 1,
            title: "before".into(),
            ..Default::default()
        };
        video.comments.push(Comment {
            rpid: 1,
            ..Default::default()
        });
        let path = export_item_for(root.path(), &video, 123).unwrap();
        assert_eq!(
            path,
            root.path()
                .join("archive-v1/123/comments/123/videos/42.json")
        );
        video.title = "after".into();
        video.comments.push(Comment {
            rpid: 2,
            ..Default::default()
        });
        assert_eq!(export_item_for(root.path(), &video, 123).unwrap(), path);
        let saved = load_doc(&path).unwrap();
        assert_eq!(saved.videos.len(), 1);
        assert_eq!(saved.videos[0].comments.len(), 2);
        for (ty, name) in [
            (11, "dynamics"),
            (17, "reposts"),
            (12, "articles"),
            (14, "audio"),
        ] {
            video.comment_type = ty;
            assert_eq!(
                item_path_for(root.path(), &video, 123).unwrap(),
                root.path()
                    .join(format!("archive-v1/123/comments/123/{name}/42.json"))
            );
        }
        video.owner_mid = 0;
        assert!(item_path_for(root.path(), &video, 123).is_err());
    }
    #[test]
    fn corrupt_archive_is_never_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let video = Video {
            aid: 42,
            owner_mid: 123,
            ..Default::default()
        };
        let path = item_path_for(root.path(), &video, 123).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"broken").unwrap();
        assert!(export_item_for(root.path(), &video, 123).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"broken");
    }
}

pub struct UserImport {
    pub doc: ArchiveDoc,
    pub loaded: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}

/// Only inspect this author's five export categories. Never modify source archives.
#[cfg(test)]
pub fn load_user_exports(root: &Path, uid: u64) -> UserImport {
    load_user_exports_with_workers(root, uid, crate::local_read::workers())
}

pub fn load_user_exports_with_workers(root: &Path, uid: u64, workers: usize) -> UserImport {
    let mut result = UserImport {
        doc: ArchiveDoc::default(),
        loaded: 0,
        skipped: 0,
        errors: vec![],
    };
    if uid == 0 {
        result.errors.push("尚未验证账号，未读取历史导出".into());
        return result;
    }
    let base = crate::archive::root(root)
        .join(uid.to_string())
        .join("comments");
    let authors = match std::fs::read_dir(&base) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return result,
        Err(e) => {
            result.errors.push(e.to_string());
            return result;
        }
    };
    let mut videos = Vec::new();
    let mut authors: Vec<_> = authors
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_type()
                .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
        })
        .collect();
    authors.sort_by_key(|author| author.file_name());
    for author in authors {
        let author_uid = author
            .file_name()
            .to_string_lossy()
            .parse::<u64>()
            .unwrap_or(0);
        if author_uid == 0 {
            continue;
        }
        for category_name in ["videos", "dynamics", "reposts", "articles", "audio"] {
            let Ok(entries) = std::fs::read_dir(author.path().join(category_name)) else {
                continue;
            };
            let mut paths: Vec<_> = entries
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_ok_and(|t| t.is_file() && !t.is_symlink()))
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|s| s == "json"))
                .collect();
            paths.sort();
            for batch in paths.chunks(32) {
                let parsed =
                    crate::local_read::map_ordered(batch, workers, |path| -> Result<ArchiveDoc> {
                        let archive = crate::archive::Envelope::parse(&std::fs::read(&path)?)?;
                        if archive.meta["account_uid"].as_str() != Some(uid.to_string().as_str()) {
                            return Err(anyhow!("归档账号不匹配"));
                        }
                        crate::archive::comments(&archive)
                    });
                for (path, parsed) in batch.iter().zip(parsed) {
                    match parsed {
                        Ok(doc)
                            if !doc.videos.is_empty()
                                && doc.videos.iter().all(|v| {
                                    v.owner_mid == author_uid
                                        && category(v) == category_name
                                        && v.aid > 0
                                }) =>
                        {
                            videos.extend(doc.videos);
                            result.loaded += 1;
                        }
                        _ => {
                            result.skipped += 1;
                            result.errors.push(format!(
                                "跳过无效或账号/作者/分类不匹配的历史文件 {}",
                                path.display()
                            ));
                        }
                    }
                }
            }
        }
    }
    result.doc.merge_videos(videos);
    result
}

#[cfg(test)]
mod user_import_tests {
    use super::*;
    use crate::model::Comment;
    #[test]
    fn parallel_comment_load_matches_serial_results() {
        let root = tempfile::tempdir().unwrap();
        for aid in 1..=8 {
            let video = Video {
                aid,
                owner_mid: 12,
                r#type: "video".into(),
                comments: vec![Comment {
                    rpid: aid * 10,
                    ..Default::default()
                }],
                ..Default::default()
            };
            export_item_for(root.path(), &video, 12).unwrap();
        }
        let serial = load_user_exports_with_workers(root.path(), 12, 1);
        let parallel = load_user_exports_with_workers(root.path(), 12, 4);
        assert_eq!(serial.loaded, 8);
        assert_eq!(serial.loaded, parallel.loaded);
        assert_eq!(serial.errors, parallel.errors);
        assert_eq!(
            serde_json::to_value(serial.doc.videos).unwrap(),
            serde_json::to_value(parallel.doc.videos).unwrap()
        );
    }

    #[test]
    fn imports_only_verified_author_and_all_categories_idempotently() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(load_user_exports(root.path(), 123).loaded, 0);
        for (ty, id) in [(1, 1), (11, 2), (17, 3), (12, 4), (14, 5)] {
            export_item_for(
                root.path(),
                &Video {
                    aid: id,
                    owner_mid: 123,
                    comment_type: ty,
                    comments: vec![Comment {
                        rpid: id,
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                123,
            )
            .unwrap();
        }
        export_item_for(
            root.path(),
            &Video {
                aid: 99,
                owner_mid: 456,
                ..Default::default()
            },
            456,
        )
        .unwrap();
        let a = load_user_exports(root.path(), 123);
        assert_eq!(a.loaded, 5);
        assert_eq!(a.doc.videos.len(), 5);
        let mut merged = a.doc;
        for video in load_user_exports(root.path(), 123).doc.videos {
            merged.merge_video(video);
        }
        assert_eq!(merged.stats.comments, 5);
        assert_eq!(merged.videos.len(), 5);
        let bad = root
            .path()
            .join("archive-v1/123/comments/123/videos/bad.json");
        std::fs::write(&bad, b"broken").unwrap();
        let mismatch = root
            .path()
            .join("archive-v1/123/comments/123/videos/other.json");
        let mut doc = ArchiveDoc::default();
        doc.merge_video(Video {
            aid: 100,
            owner_mid: 456,
            ..Default::default()
        });
        std::fs::write(&mismatch, to_json(&doc).unwrap()).unwrap();
        let result = load_user_exports(root.path(), 123);
        assert_eq!(result.loaded, 5);
        assert_eq!(result.skipped, 2);
        assert_eq!(std::fs::read(bad).unwrap(), b"broken");
    }
}

#[cfg(test)]
mod continuous_archive_tests {
    use super::*;
    use crate::model::Comment;
    fn video(ty: u32) -> Video {
        Video {
            aid: 42,
            owner_mid: 123,
            r#type: match ty {
                11 | 17 => "dynamic",
                12 => "article",
                14 => "audio",
                _ => "video",
            }
            .into(),
            comment_type: ty,
            comments: vec![Comment {
                rpid: 7,
                ..Default::default()
            }],
            ..Default::default()
        }
    }
    #[test]
    fn all_five_types_save_separate_objects_before_advancing_the_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let videos: Vec<_> = [1, 11, 17, 12, 14].into_iter().map(video).collect();
        let checkpoint = dir.path().join("checkpoint.json");
        let count = persist_comments(dir.path(), 12, &videos, || {
            for v in &videos {
                let path = item_path_for(dir.path(), v, 12)?;
                let saved = load_doc(&path)?;
                assert_eq!(saved.videos[0].reply_type(), v.reply_type());
                assert_eq!(saved.stats.comments, 1);
            }
            std::fs::write(&checkpoint, b"advanced")?;
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 5);
        assert_eq!(std::fs::read(checkpoint).unwrap(), b"advanced");
        let paths: std::collections::HashSet<_> = videos
            .iter()
            .map(|v| item_path_for(dir.path(), v, 12).unwrap())
            .collect();
        assert_eq!(paths.len(), 5);
        assert_ne!(
            item_path_for(dir.path(), &videos[0], 12).unwrap(),
            item_path_for(dir.path(), &videos[0], 13).unwrap()
        );
    }
    #[test]
    fn unreadable_existing_archive_does_not_advance_or_overwrite_any_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let v = video(1);
        let target = item_path_for(dir.path(), &v, 12).unwrap();
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"broken").unwrap();
        let mut advanced = false;
        assert!(persist_comments(dir.path(), 12, &[v], || {
            advanced = true;
            Ok(())
        })
        .is_err());
        assert!(!advanced);
        assert_eq!(std::fs::read(target).unwrap(), b"broken");
    }
    #[test]
    fn checkpoint_failure_can_replay_saved_content_without_duplicate_comments() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = video(1);
        let checkpoint = dir.path().join("checkpoint.json");
        std::fs::write(&checkpoint, b"previous").unwrap();
        assert!(
            persist_comments(dir.path(), 12, std::slice::from_ref(&v), || Err(anyhow!(
                "checkpoint unavailable"
            )))
            .is_err()
        );
        assert_eq!(std::fs::read(&checkpoint).unwrap(), b"previous");
        v.comments.push(Comment {
            rpid: 8,
            ..Default::default()
        });
        persist_comments(dir.path(), 12, std::slice::from_ref(&v), || {
            std::fs::write(&checkpoint, b"next")?;
            Ok(())
        })
        .unwrap();
        persist_comments(dir.path(), 12, std::slice::from_ref(&v), || Ok(())).unwrap();
        assert_eq!(
            load_doc(&item_path_for(dir.path(), &v, 12).unwrap())
                .unwrap()
                .stats
                .comments,
            2
        );
    }
    #[test]
    fn a_foreign_account_file_in_the_target_directory_is_not_merged() {
        let dir = tempfile::tempdir().unwrap();
        let v = video(1);
        let source = export_item_for(dir.path(), &v, 13).unwrap();
        let target = item_path_for(dir.path(), &v, 12).unwrap();
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(source, &target).unwrap();
        let original = std::fs::read(&target).unwrap();
        assert!(persist_comments(dir.path(), 12, &[v], || panic!("must not advance")).is_err());
        assert_eq!(std::fs::read(target).unwrap(), original);
    }
}
