use crate::model::{ArchiveDoc, Comment, Reply, Video};
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::path::Path;

/// 导出为 JSON（唯一格式）。
/// 评论是主评论挂楼中楼的层级树，JSON 天然贴合；文本里常见的引号 / 反斜杠 / emoji
/// 按 JSON 规则转义即可，消费方（网页、脚本、表格）拿到就能直接读。
pub fn to_json(doc: &ArchiveDoc) -> Result<String> {
    Ok(serde_json::to_string_pretty(doc)?)
}

/// 从任意来源的 JSON 中提取视频与评论：
/// 1) 本工具导出的文档  2) 浏览器里抓到的接口响应  3) 裸的 replies 数组
pub fn import_json(text: &str, fallback_name: &str) -> Result<Vec<Video>> {
    let v: Value = serde_json::from_str(text).map_err(|e| anyhow!("JSON 解析失败：{}", e))?;
    import_value(v, fallback_name)
}

pub fn import_value(v: Value, fallback_name: &str) -> Result<Vec<Video>> {
    // 1) 本工具格式
    if let Some(list) = v.get("videos").and_then(|x| x.as_array()) {
        let mut out = Vec::new();
        for item in list {
            if let Ok(mut video) = serde_json::from_value::<Video>(item.clone()) {
                if video.aid == 0 {
                    video.aid = guess_aid(item);
                }
                if video.title.is_empty() {
                    video.title = fallback_name.to_string();
                }
                if video.url.is_empty() {
                    video.url = Video::url_for(&video.bvid, video.aid);
                }
                out.push(video);
            }
        }
        if !out.is_empty() {
            return Ok(out);
        }
    }

    // 2) 接口响应：{ code, data: { replies: [...], top_replies, cursor } }
    if let Some(data) = v.get("data") {
        if data.get("replies").is_some() || data.get("top_replies").is_some() {
            return Ok(vec![video_from_api(data, fallback_name)]);
        }
    }

    // 3) 裸数组
    if let Some(arr) = v.as_array() {
        let wrapped = serde_json::json!({ "replies": arr });
        return Ok(vec![video_from_api(&wrapped, fallback_name)]);
    }

    Err(anyhow!("无法识别的 JSON 结构"))
}

fn guess_aid(v: &Value) -> u64 {
    v.get("aid")
        .and_then(|x| x.as_u64())
        .or_else(|| {
            v.get("comments")
                .and_then(|c| c.as_array())
                .and_then(|a| a.first())
                .and_then(|c| c.get("oid"))
                .and_then(|x| x.as_u64())
        })
        .unwrap_or(0)
}

fn video_from_api(data: &Value, fallback_name: &str) -> Video {
    let mut video = Video::default();
    let mut seen: Vec<u64> = Vec::new();

    for key in ["top_replies", "replies"] {
        if let Some(list) = data.get(key).and_then(|x| x.as_array()) {
            for item in list {
                let rpid = item.get("rpid").and_then(|x| x.as_u64()).unwrap_or(0);
                if rpid == 0 || seen.contains(&rpid) {
                    continue;
                }
                seen.push(rpid);
                if video.aid == 0 {
                    video.aid = item.get("oid").and_then(|x| x.as_u64()).unwrap_or(0);
                }
                if let Some(c) = comment_from_value(item, key == "top_replies") {
                    video.comments.push(c);
                }
            }
        }
    }
    if video.aid == 0 {
        video.aid = guess_aid(data);
    }
    video.title = fallback_name.to_string();
    video.url = Video::url_for(&video.bvid, video.aid);
    video.comment_total = video.comments.len() as u64;
    video.comments.sort_by(|a, b| {
        b.is_pinned
            .cmp(&a.is_pinned)
            .then_with(|| b.ctime.cmp(&a.ctime))
    });
    video
}

fn avatar_of(member: &Value) -> String {
    let raw = member
        .get("avatar")
        .and_then(|x| x.as_str())
        .filter(|s| !s.trim().is_empty())
        .or_else(|| member.get("face").and_then(|x| x.as_str()))
        .unwrap_or("")
        .trim();
    match raw.strip_prefix("http://") {
        Some(rest) => format!("https://{}", rest),
        None => raw.to_string(),
    }
}

fn is_vip_of(member: &Value) -> bool {
    let vip = member.get("vip");
    match vip {
        Some(v) => {
            v.get("vipStatus").and_then(|x| x.as_i64()).unwrap_or(0) == 1
                || v.get("vipType").and_then(|x| x.as_i64()).unwrap_or(0) > 0
                || v.get("accessStatus").and_then(|x| x.as_i64()).unwrap_or(0) == 1
        }
        None => false,
    }
}

fn location_of(v: &Value) -> String {
    let raw = v
        .get("reply_control")
        .and_then(|c| c.get("location").or_else(|| c.get("ip_location")))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim();
    raw.trim_start_matches("IP属地")
        .trim_start_matches(['：', ':', ' '])
        .trim()
        .to_string()
}

fn pictures_of(v: &Value) -> Vec<String> {
    v.get("content")
        .and_then(|c| c.get("pictures"))
        .and_then(|p| p.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|p| {
                    let src = p
                        .get("img_src")
                        .or_else(|| p.get("img_url"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .trim();
                    if src.is_empty() {
                        None
                    } else {
                        Some(match src.strip_prefix("http://") {
                            Some(rest) => format!("https://{}", rest),
                            None => src.to_string(),
                        })
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn comment_from_value(v: &Value, pinned: bool) -> Option<Comment> {
    let rpid = v.get("rpid").and_then(|x| x.as_u64())?;
    let member = v.get("member")?;
    let content = v
        .get("content")
        .and_then(|c| c.get("message"))
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    let pictures = pictures_of(v);
    if content.is_empty() && pictures.is_empty() {
        return None;
    }
    let ctime = v.get("ctime").and_then(|x| x.as_i64()).unwrap_or(0);
    let mid = member
        .get("mid")
        .map(|m| match m {
            Value::String(s) => s.parse::<u64>().unwrap_or(0),
            Value::Number(n) => n.as_u64().unwrap_or(0),
            _ => 0,
        })
        .unwrap_or(0);
    Some(Comment {
        rpid,
        mid,
        uname: member
            .get("uname")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        avatar: avatar_of(member),
        level: member
            .get("level_info")
            .and_then(|l| l.get("current_level"))
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as u32,
        is_vip: is_vip_of(member),
        location: location_of(v),
        pictures,
        is_up: false,
        is_pinned: pinned,
        content,
        like: v.get("like").and_then(|x| x.as_u64()).unwrap_or(0),
        ctime,
        ctime_text: crate::model::ts_text(ctime),
        reply_count: v.get("rcount").and_then(|x| x.as_u64()).unwrap_or_else(|| {
            v.get("replies")
                .and_then(|r| r.as_array())
                .map(|a| a.len() as u64)
                .unwrap_or(0)
        }),
        replies: v
            .get("replies")
            .and_then(|r| r.as_array())
            .map(|list| {
                list.iter()
                    .filter_map(|item| {
                        let rpid = item.get("rpid").and_then(|x| x.as_u64())?;
                        let content = item
                            .get("content")
                            .and_then(|c| c.get("message"))
                            .and_then(|m| m.as_str())
                            .unwrap_or("")
                            .to_string();
                        let pictures = pictures_of(item);
                        if content.is_empty() && pictures.is_empty() {
                            return None;
                        }
                        let member = item.get("member");
                        let mid = member
                            .and_then(|m| m.get("mid"))
                            .map(|m| match m {
                                Value::String(s) => s.parse::<u64>().unwrap_or(0),
                                Value::Number(n) => n.as_u64().unwrap_or(0),
                                _ => 0,
                            })
                            .unwrap_or(0);
                        let ctime = item.get("ctime").and_then(|x| x.as_i64()).unwrap_or(0);
                        Some(Reply {
                            rpid,
                            mid,
                            uname: member
                                .and_then(|m| m.get("uname"))
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string(),
                            avatar: member.map(avatar_of).unwrap_or_default(),
                            level: member
                                .and_then(|m| m.get("level_info"))
                                .and_then(|l| l.get("current_level"))
                                .and_then(|x| x.as_u64())
                                .unwrap_or(0) as u32,
                            is_vip: member.map(is_vip_of).unwrap_or(false),
                            location: location_of(item),
                            pictures,
                            is_up: false,
                            content,
                            like: item.get("like").and_then(|x| x.as_u64()).unwrap_or(0),
                            ctime,
                            ctime_text: crate::model::ts_text(ctime),
                            parent: item.get("parent").and_then(|x| x.as_u64()).unwrap_or(0),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

pub fn write_file(path: &Path, content: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    std::fs::write(path, content)?;
    Ok(())
}

/// 读取导出的 JSON 文件
pub fn load_doc(path: &Path) -> Result<ArchiveDoc> {
    let text = std::fs::read_to_string(path)?;
    if !text.trim_start().starts_with('{') {
        return Err(anyhow!(
            "这个文件不是 JSON（本工具只导出 JSON，扩展名 .json）"
        ));
    }
    let mut doc: ArchiveDoc =
        serde_json::from_str(&text).map_err(|e| anyhow!("JSON 解析失败：{}", e))?;
    doc.recount();
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
    if video.owner_mid == 0 || video.aid == 0 {
        return Err(anyhow!("缺少作者 UID 或稿件 ID，跳过自动导出"));
    }
    Ok(root
        .join(video.owner_mid.to_string())
        .join(category(video))
        .join(format!("{}.json", video.aid)))
}

/// One stable file per author/type/object. Never overwrite an unreadable archive.
pub fn export_item(root: &Path, video: &Video) -> Result<std::path::PathBuf> {
    let path = item_path(root, video)?;
    let mut doc = if path.exists() {
        let old = load_doc(&path)?;
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
    crate::secure::write_config(&path, &serde_json::to_value(&doc)?)?;
    Ok(path)
}

#[cfg(test)]
mod item_export_tests {
    use super::*;
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
        let path = export_item(root.path(), &video).unwrap();
        assert_eq!(path, root.path().join("123/videos/42.json"));
        video.title = "after".into();
        video.comments.push(Comment {
            rpid: 2,
            ..Default::default()
        });
        assert_eq!(export_item(root.path(), &video).unwrap(), path);
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
                item_path(root.path(), &video).unwrap(),
                root.path().join(format!("123/{name}/42.json"))
            );
        }
        video.owner_mid = 0;
        assert!(item_path(root.path(), &video).is_err());
    }
    #[test]
    fn corrupt_archive_is_never_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let video = Video {
            aid: 42,
            owner_mid: 123,
            ..Default::default()
        };
        let path = item_path(root.path(), &video).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"broken").unwrap();
        assert!(export_item(root.path(), &video).is_err());
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
pub fn load_user_exports(root: &Path, uid: u64) -> UserImport {
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
    for category_name in ["videos", "dynamics", "reposts", "articles", "audio"] {
        let dir = root.join(uid.to_string()).join(category_name);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                result
                    .errors
                    .push(format!("无法读取历史导出目录 {}", dir.display()));
                continue;
            }
        };
        let mut paths = vec![];
        for entry in entries {
            match entry {
                Ok(e)
                    if e.path().extension().is_some_and(|x| {
                        x.to_str().is_some_and(|s| s.eq_ignore_ascii_case("json"))
                    }) =>
                {
                    paths.push(e.path())
                }
                Ok(_) => (),
                Err(_) => result
                    .errors
                    .push(format!("无法枚举历史导出目录 {}", dir.display())),
            }
        }
        paths.sort();
        for path in paths {
            match load_doc(&path) {
                Ok(doc)
                    if !doc.videos.is_empty()
                        && doc.videos.iter().all(|v| {
                            v.owner_mid == uid && category(v) == category_name && v.aid > 0
                        }) =>
                {
                    for video in doc.videos {
                        result.doc.merge_video(video);
                    }
                    result.loaded += 1;
                }
                _ => {
                    result.skipped += 1;
                    result.errors.push(format!(
                        "跳过无效或作者/分类不匹配的历史文件 {}",
                        path.display()
                    ));
                }
            }
        }
    }
    result
}

#[cfg(test)]
mod user_import_tests {
    use super::*;
    #[test]
    fn imports_only_verified_author_and_all_categories_idempotently() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(load_user_exports(root.path(), 123).loaded, 0);
        for (ty, id) in [(1, 1), (11, 2), (17, 3), (12, 4), (14, 5)] {
            export_item(
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
            )
            .unwrap();
        }
        export_item(
            root.path(),
            &Video {
                aid: 99,
                owner_mid: 456,
                ..Default::default()
            },
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
        let bad = root.path().join("123/videos/bad.json");
        std::fs::write(&bad, b"broken").unwrap();
        let mismatch = root.path().join("123/videos/other.json");
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

pub struct SplitReport {
    pub saved: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}
pub fn split_import(files: &[std::path::PathBuf], root: &Path) -> SplitReport {
    let mut report = SplitReport {
        saved: 0,
        failed: 0,
        errors: vec![],
    };
    let mut merged = ArchiveDoc::default();
    for path in files {
        let parsed = std::fs::read_to_string(path)
            .map_err(anyhow::Error::from)
            .and_then(|text| import_json(&text, "imported"));
        match parsed {
            Ok(videos) => {
                for video in videos {
                    merged.merge_video(video);
                }
            }
            Err(_) => {
                report.failed += 1;
                report
                    .errors
                    .push(format!("无法拆解源文件 {}，原文件已保留", path.display()));
            }
        }
    }
    let sources = files
        .iter()
        .filter_map(|p| p.canonicalize().ok())
        .collect::<Vec<_>>();
    for video in &merged.videos {
        let result = item_path(root, video).and_then(|target| {
            if target
                .canonicalize()
                .ok()
                .is_some_and(|p| sources.contains(&p))
            {
                return Err(anyhow!("目标与源文件相同，未改写源文件"));
            }
            export_item(root, video)
        });
        match result {
            Ok(_) => report.saved += 1,
            Err(e) => {
                report.failed += 1;
                report
                    .errors
                    .push(format!("稿件 {} 拆解导出失败：{}", video.aid, e));
            }
        }
    }
    report
}

#[cfg(test)]
mod split_tests {
    use super::*;
    #[test]
    fn splits_by_existing_export_layout_and_preserves_source() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("exports");
        let source = dir.path().join("all.json");
        let mut doc = ArchiveDoc::default();
        for (ty, id) in [(1, 42), (11, 43), (17, 44), (12, 45), (14, 46)] {
            doc.merge_video(Video {
                owner_mid: 123,
                aid: id,
                comment_type: ty,
                comments: vec![Comment {
                    rpid: id,
                    ..Default::default()
                }],
                ..Default::default()
            });
        }
        let text = to_json(&doc).unwrap();
        std::fs::write(&source, &text).unwrap();
        let first = split_import(&[source.clone()], &root);
        assert_eq!((first.saved, first.failed), (5, 0));
        let again = split_import(&[source.clone()], &root);
        assert_eq!(again.saved, 5);
        let imported = load_user_exports(&root, 123);
        assert_eq!(imported.doc.stats.comments, 5);
        assert_eq!(std::fs::read_to_string(&source).unwrap(), text);
        let same = split_import(&[root.join("123/videos/42.json")], &root);
        assert_eq!(same.failed, 1);
    }
}
