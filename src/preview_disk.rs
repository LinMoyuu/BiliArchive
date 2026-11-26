//! Read-only access to the desktop's atomically written archive files.
use crate::{archive, messages, model::ArchiveDoc, preview_process::Scope};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, RwLock,
    },
    time::SystemTime,
};

const MAX_CACHED_SOURCE: u64 = 2 * 1024 * 1024;
const MAX_CACHED_LIST: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Source {
    path: PathBuf,
    modified: Option<SystemTime>,
    size: u64,
}

fn source(path: PathBuf) -> Option<Source> {
    let meta = fs::symlink_metadata(&path).ok()?;
    meta.is_file().then(|| Source {
        path,
        modified: meta.modified().ok(),
        size: meta.len(),
    })
}

fn directories(path: &Path) -> Vec<PathBuf> {
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|e| e.path())
        .collect()
}

fn json_sources(dir: &Path) -> Vec<Source> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .filter_map(source)
        .collect()
}

fn comment_sources(root: &Path, uid: u64, aid: Option<u64>) -> Vec<Source> {
    let account = if uid == 0 {
        "public".into()
    } else {
        uid.to_string()
    };
    let dir = archive::root(root).join(account).join("comments");
    let mut sources = Vec::new();
    if root.as_os_str().is_empty() {
        return sources;
    }
    for author in directories(&dir) {
        if author.file_name().is_some_and(|name| name == "public") {
            sources.extend(json_sources(&author));
        }
        for category in ["videos", "dynamics", "reposts", "articles", "audio"] {
            let category = author.join(category);
            if let Some(aid) = aid {
                sources.extend(source(category.join(format!("{aid}.json"))));
            } else {
                sources.extend(json_sources(&category));
            }
        }
    }
    sources.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    sources
}

fn comment_file(file: &Source, uid: u64) -> Option<ArchiveDoc> {
    let envelope = archive::Envelope::parse(&fs::read(&file.path).ok()?).ok()?;
    let owner = envelope.meta["account_uid"]
        .as_str()
        .and_then(|id| id.parse().ok())
        .unwrap_or(0);
    if owner != uid {
        return None;
    }
    let doc = archive::comments(&envelope).ok()?;
    let parent = file.path.parent()?;
    if parent.file_name().is_some_and(|name| name == "public") {
        return Some(doc);
    }
    let owner_uid: u64 = parent.parent()?.file_name()?.to_str()?.parse().ok()?;
    let aid: u64 = file.path.file_stem()?.to_str()?.parse().ok()?;
    doc.videos
        .iter()
        .all(|v| v.owner_mid == owner_uid && v.aid == aid && aid > 0)
        .then_some(doc)
}

fn comment_doc(scope: &Scope, sources: &[Source]) -> ArchiveDoc {
    if let Some(path) = &scope.standalone {
        return crate::export::load_doc(path).unwrap_or_default();
    }
    let mut doc = ArchiveDoc::default();
    let mut resources = serde_json::Map::new();
    let mut videos = Vec::new();
    for mut file in sources
        .iter()
        .filter_map(|file| comment_file(file, scope.uid))
    {
        if let Some(table) = file.archive["resources"].as_object_mut() {
            resources.append(table);
        }
        videos.append(&mut file.videos);
    }
    doc.archive = json!({"resource_root": archive::root(&scope.root), "account_uid": scope.uid.to_string(), "resources": resources});
    doc.merge_videos(videos);
    doc
}

pub fn comment_projection(root: &Path, uid: u64) -> ArchiveDoc {
    let sources = comment_sources(root, uid, None);
    read_index(root, uid, &sources)
        .map_or_else(|| comment_catalog(&sources, uid).0, |index| index.doc)
}

fn desktop_scope(root: &Path, uid: u64) -> Scope {
    Scope {
        root: root.into(),
        uid,
        offline: false,
        precache: false,
        account: Default::default(),
        standalone: None,
    }
}

pub fn read_comments(root: &Path, uid: u64) -> ArchiveDoc {
    View::new(desktop_scope(root, uid)).comments(None)
}

pub fn small_comment_archive(root: &Path, uid: u64) -> bool {
    comment_sources(root, uid, None)
        .iter()
        .map(|file| file.size)
        .sum::<u64>()
        <= MAX_CACHED_SOURCE
}

pub fn desktop_message_view(root: &Path, uid: u64) -> (Value, bool) {
    let view = View::new(desktop_scope(root, uid));
    let summary = view.message_summary(None);
    let likes = view.messages("likes", "");
    let resumable = messages::has_resumable_task(uid, &likes.lock().unwrap()).unwrap_or(false);
    (summary, resumable)
}

#[derive(Serialize, Deserialize)]
struct CommentIndex {
    sources: Vec<Source>,
    doc: ArchiveDoc,
    rows: Vec<Value>,
}

fn index_path(root: &Path, uid: u64) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    let key = archive::hash(&format!("{}:{uid}", root.display()));
    Some(
        crate::checkpoint::data_dir()
            .ok()?
            .join("preview-index-v2")
            .join(format!("{key}.json")),
    )
}

fn read_index(root: &Path, uid: u64, sources: &[Source]) -> Option<CommentIndex> {
    let path = index_path(root, uid)?;
    let index: CommentIndex =
        serde_json::from_reader(std::io::BufReader::new(fs::File::open(path).ok()?)).ok()?;
    (index.sources == sources).then_some(index)
}

pub fn desktop_projection(root: &Path, uid: u64) -> ArchiveDoc {
    let sources = comment_sources(root, uid, None);
    if let Some(index) = read_index(root, uid, &sources) {
        return index.doc;
    }
    let (doc, rows) = comment_catalog(&sources, uid);
    let index = CommentIndex { sources, doc, rows };
    if !index.sources.is_empty() {
        if let Some(path) = index_path(root, uid) {
            if let Ok(data) = serde_json::to_value(&index) {
                let _ = crate::secure::write_config(&path, &data);
            }
        }
    }
    index.doc
}

fn first_comment_time(video: &crate::model::Video) -> Option<String> {
    use chrono::{Local, TimeZone};
    video
        .comments
        .iter()
        .filter_map(|comment| {
            let text = comment.ctime_text.trim();
            chrono::DateTime::parse_from_rfc3339(text)
                .ok()
                .map(|t| t.timestamp_millis())
                .or_else(|| {
                    chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S")
                        .ok()
                        .and_then(|t| Local.from_local_datetime(&t).earliest())
                        .map(|t| t.timestamp_millis())
                })
                .or_else(|| {
                    (comment.ctime > 0).then(|| {
                        if comment.ctime < 100_000_000_000 {
                            comment.ctime * 1000
                        } else {
                            comment.ctime
                        }
                    })
                })
        })
        .min()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|time| time.to_rfc3339())
}

fn comment_catalog_row(video: &crate::model::Video) -> Value {
    let mut row = serde_json::to_value(video.without_comments()).unwrap_or_default();
    row["first_comment_time"] = json!(first_comment_time(video));
    row["fetched"] = json!(video.comments.len());
    row["replies"] = json!(video.total_replies());
    row["display_title"] = json!(video.display_title());
    row
}

// Keep per-file statistics and row metadata while discarding comment bodies.
fn comment_catalog(sources: &[Source], uid: u64) -> (ArchiveDoc, Vec<Value>) {
    let mut catalog = ArchiveDoc::default();
    let mut rows = Vec::new();
    let mut positions = std::collections::HashSet::new();
    for mut doc in sources.iter().filter_map(|file| comment_file(file, uid)) {
        for mut video in doc.videos.drain(..) {
            if !positions.insert((video.reply_type(), video.aid)) {
                continue;
            }
            rows.push(comment_catalog_row(&video));
            catalog.stats.comments += video.comments.len();
            for comment in &video.comments {
                catalog.stats.likes +=
                    comment.like + comment.replies.iter().map(|r| r.like).sum::<u64>();
                catalog.stats.replies += comment.replies.len();
                catalog.stats.with_location += usize::from(!comment.location.is_empty())
                    + comment
                        .replies
                        .iter()
                        .filter(|r| !r.location.is_empty())
                        .count();
            }
            catalog.stats.deleted_videos += usize::from(video.is_hidden());
            catalog.stats.dynamics += usize::from(video.is_dynamic());
            catalog.stats.videos += usize::from(!video.is_dynamic());
            video.comments = Vec::new();
            catalog.videos.push(video);
        }
    }
    (catalog, rows)
}

fn message_sources(scope: &Scope, ty: &str, session: &str) -> Vec<Source> {
    if scope.uid == 0 {
        return Vec::new();
    }
    let root = messages::base(&scope.root, scope.uid);
    let mut files: Vec<_> = messages::TYPES
        .iter()
        .copied()
        .filter(|name| *name != "private" && (ty == "all" || *name == ty))
        .filter_map(|name| source(root.join(format!("{name}.json"))))
        .collect();
    if ty == "all" || ty == "private" || ty == "groups" {
        let dir = root.join("private");
        if session.is_empty() {
            files.extend(json_sources(&dir));
        } else if session.split_once('_').is_some_and(|(prefix, uid)| {
            matches!(prefix, "user" | "group") && uid.parse::<u64>().is_ok()
        }) {
            files.extend(source(dir.join(format!("{session}.json"))));
        }
    }
    files.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    files
}

fn message_file(file: &Source, uid: u64) -> Option<messages::Doc> {
    let envelope = archive::Envelope::parse(&fs::read(&file.path).ok()?).ok()?;
    let doc = archive::messages(&envelope).ok()?;
    doc.validate(uid).ok()?;
    Some(doc)
}

type MessageSnapshot = Arc<Mutex<messages::Store>>;
type Cached<T> = Mutex<Option<(Vec<Source>, T)>>;

type Subjects = std::collections::BTreeMap<String, crate::message_subjects::Subject>;

struct Reads {
    subjects: Cached<Subjects>,
    list: Cached<Arc<Value>>,
    summary: Cached<(Value, messages::AttachmentInventory)>,
    messages: Cached<MessageSnapshot>,
}

pub struct View {
    pub scope: Scope,
    reads: Reads,
}

impl View {
    fn new(scope: Scope) -> Self {
        Self {
            scope,
            reads: Reads {
                subjects: Mutex::new(None),
                list: Mutex::new(None),
                summary: Mutex::new(None),
                messages: Mutex::new(None),
            },
        }
    }

    pub fn image_scope(&self) -> Option<crate::web_images::Scope> {
        if self.scope.root.as_os_str().is_empty() {
            return None;
        }
        crate::web_images::Scope::new(archive::root(&self.scope.root), &self.scope.uid.to_string())
    }

    pub fn comments(&self, aid: Option<u64>) -> ArchiveDoc {
        comment_doc(
            &self.scope,
            &comment_sources(&self.scope.root, self.scope.uid, aid),
        )
    }

    pub fn search(&self, query: &str) -> Vec<String> {
        let needle = query.to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        let matches = |video: &crate::model::Video| {
            let hit = |text: &str| text.to_lowercase().contains(&needle);
            [&video.title, &video.text, &video.orig_text, &video.bvid]
                .into_iter()
                .any(|text| hit(text))
                || video.comments.iter().any(|comment| {
                    hit(&comment.content)
                        || hit(&comment.uname)
                        || comment
                            .replies
                            .iter()
                            .any(|reply| hit(&reply.content) || hit(&reply.uname))
                })
        };
        let mut keys = Vec::new();
        let mut append = |doc: ArchiveDoc| {
            keys.extend(
                doc.videos
                    .iter()
                    .filter(|video| matches(video))
                    .map(|video| {
                        let ty = if video.r#type.is_empty() {
                            "video"
                        } else {
                            &video.r#type
                        };
                        format!("{ty}-{}", video.aid)
                    }),
            );
        };
        if self.scope.standalone.is_some() {
            append(self.comments(None));
        } else {
            for doc in comment_sources(&self.scope.root, self.scope.uid, None)
                .iter()
                .filter_map(|file| comment_file(file, self.scope.uid))
            {
                append(doc);
            }
        }
        keys
    }

    pub fn list(&self) -> Arc<Value> {
        let scope = &self.scope;
        let files = if let Some(path) = &scope.standalone {
            source(path.clone()).into_iter().collect()
        } else {
            comment_sources(&scope.root, scope.uid, None)
        };
        let mut cache = self.reads.list.lock().unwrap();
        if let Some((previous, snapshot)) = &*cache {
            if *previous == files {
                return snapshot.clone();
            }
        }
        let (doc, rows) = if scope.standalone.is_some() {
            let doc = comment_doc(scope, &[]);
            let rows = doc.videos.iter().map(comment_catalog_row).collect();
            (doc, rows)
        } else if let Some(index) = read_index(&scope.root, scope.uid, &files) {
            (index.doc, index.rows)
        } else {
            comment_catalog(&files, scope.uid)
        };
        let mut value = json!({"meta": doc.meta, "stats": doc.stats, "videos": rows});
        value["paged_details"] = json!(true);
        archive::string_ids(&mut value);
        let value = Arc::new(value);
        *cache = (scope.precache && value.to_string().len() <= MAX_CACHED_LIST)
            .then(|| (files, value.clone()));
        value
    }

    pub fn post_summaries(&self) -> Value {
        let snapshot = self.list();
        let posts: Vec<Value> = snapshot["videos"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|post| {
                let aid = post["aid"]
                    .as_str()
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(0);
                json!({
                    "aid": aid,
                    "bvid": post["bvid"],
                    "type": post["type"],
                    "title": post["display_title"],
                    "cover": post["cover"],
                    "url": post["url"],
                    "status": post["status"],
                    "statusText": post["status_text"],
                    "publishTime": post["publish_time_text"],
                    "commentTotal": post["comment_total"],
                    "fetched": post["fetched"],
                    "replies": post["replies"],
                })
            })
            .collect();
        json!({"stats": snapshot["stats"], "exportedAt": snapshot["meta"]["exported_at"], "videos": posts})
    }

    fn message_documents(
        &self,
        ty: &str,
        session: &str,
    ) -> impl Iterator<Item = messages::Doc> + '_ {
        message_sources(&self.scope, ty, session)
            .into_iter()
            .filter_map(|file| message_file(&file, self.scope.uid))
    }

    pub fn message_summary(&self, images: Option<&crate::web_images::Cache>) -> Value {
        use std::collections::BTreeMap;
        let root = archive::root(&self.scope.root);
        let files = (self.scope.precache && images.is_some()).then(|| {
            let mut files = message_sources(&self.scope, "all", "");
            files.extend(source(
                root.join("resources")
                    .join(self.scope.uid.to_string())
                    .join("index.json"),
            ));
            files
        });
        if let Some(files) = &files {
            if let Some((old, (value, attachments))) = &*self.reads.summary.lock().unwrap() {
                if old == files {
                    let mut value = value.clone();
                    value["attachments_by_type"] = attachments.counts(&root, &self.message_uid());
                    return value;
                }
            }
        }
        let mut counts: BTreeMap<&str, usize> =
            messages::VIEW_TYPES.iter().map(|ty| (*ty, 0)).collect();
        let mut sessions = Vec::new();
        let mut known = BTreeMap::new();
        let mut resource_scan = archive::MessageResourceScan::new(&root, self.scope.uid);
        let mut attachments = messages::AttachmentInventory::default();
        for doc in self.message_documents("all", "") {
            for (ty, count) in doc.counts() {
                *counts.entry(ty).or_default() += count;
            }
            sessions.extend(messages::session_summaries(&doc));
            known.extend(crate::message_profiles::local_profiles(&doc));
            resource_scan.add(&doc);
            if images.is_some() {
                attachments.add(&doc);
            }
        }
        // Reuse notification avatars for private participants, as in the in-memory view.
        if known.values().any(|profile| profile.avatar_url.is_empty()) {
            for ty in ["replies", "mentions", "likes"] {
                for doc in self.message_documents(ty, "") {
                    crate::message_profiles::reuse_profiles(&doc, &mut known);
                }
            }
        }
        for session in &mut sessions {
            if let Some(profile) = session["talker_id"].as_str().and_then(|key| known.get(key)) {
                if session["session_type"] == 1 && !profile.avatar_url.is_empty() {
                    session["avatar_url"] = json!(profile.avatar_url);
                }
            }
        }
        let resources = resource_scan.finish();
        let total: usize = counts.values().sum();
        let saved = resources
            .values()
            .filter(|r| r["status"] == "saved")
            .count();
        let pending = resources
            .values()
            .filter(|r| r["status"] == "pending" || r["status"] == "failed")
            .count();
        let mut value = json!({"account_uid": self.message_uid(), "counts": counts, "sessions": sessions,
            "total": total, "attachments_saved": saved, "attachments_pending": pending});
        if let Some((images, scope)) = images.zip(self.image_scope()) {
            let records = resources.into_iter().collect();
            value["resource_images"] =
                Value::Object(images.get(&scope).resource_images(Some(&records)));
        }
        if images.is_some() {
            value["attachments_by_type"] = attachments.counts(&root, &self.message_uid());
        }
        if let Some(files) = files {
            let mut cached = value.clone();
            if let Some(object) = cached.as_object_mut() {
                object.remove("attachments_by_type");
            }
            let bytes = cached.to_string().len().saturating_add(attachments.cache_bytes());
            let retained = (bytes <= MAX_CACHED_LIST)
                .then_some((files, (cached, attachments)));
            *self.reads.summary.lock().unwrap() = retained;
        }
        value
    }

    pub fn message_uid(&self) -> String {
        if self.scope.uid == 0 {
            String::new()
        } else {
            self.scope.uid.to_string()
        }
    }

    pub fn private_page(
        &self,
        ty: &str,
        session: &str,
        query: &str,
        ascending: bool,
        offset: usize,
        limit: usize,
        include_groups: bool,
    ) -> Value {
        let mut page = Vec::new();
        let mut total = 0usize;
        let keep = offset.saturating_add(limit);
        for doc in self.message_documents(ty, session) {
            let items = doc.query(
                if include_groups { "all" } else { ty },
                session,
                query,
                ascending,
            );
            let known = crate::message_profiles::local_profiles(&doc);
            total += items.len();
            page.extend(items.into_iter().take(keep).map(|item| {
                (
                    item.timestamp,
                    crate::message_profiles::item_summary(item, &known),
                )
            }));
            page.sort_by(|(a, _), (b, _)| match (*a, *b) {
                (0, 0) => std::cmp::Ordering::Equal,
                (0, _) => std::cmp::Ordering::Greater,
                (_, 0) => std::cmp::Ordering::Less,
                _ if ascending => a.cmp(b),
                _ => b.cmp(a),
            });
            page.truncate(keep);
        }
        json!({"account_uid": self.message_uid(), "total": total,
            "items": page.into_iter().skip(offset).map(|(_, value)| value).collect::<Vec<_>>()})
    }

    pub fn profile(&self, uid: &str) -> Option<crate::message_profiles::Profile> {
        let session = format!("user_{uid}");
        let locate = |doc: messages::Doc| crate::message_profiles::local_profiles(&doc).remove(uid);
        let found = self
            .message_documents("private", &session)
            .find_map(locate)
            .or_else(|| self.message_documents("private", "").find_map(locate))?;
        let mut known = std::collections::BTreeMap::from([(uid.to_owned(), found)]);
        if known[uid].avatar_url.is_empty() || known[uid].name.is_empty() {
            for ty in ["replies", "mentions", "likes"] {
                for doc in self.message_documents(ty, "") {
                    crate::message_profiles::reuse_profiles(&doc, &mut known);
                }
            }
        }
        known.remove(uid)
    }

    pub fn subjects(&self, keys: &[String]) -> Subjects {
        let messages: Vec<_> = ["likes", "mentions", "replies"]
            .into_iter()
            .flat_map(|ty| message_sources(&self.scope, ty, ""))
            .collect();
        let mut sources = messages.clone();
        sources.extend(comment_sources(&self.scope.root, self.scope.uid, None));

        let select = |all: &Subjects| {
            keys.iter()
                .filter_map(|key| all.get(key).map(|s| (key.clone(), s.clone())))
                .collect()
        };
        let mut cache = self.reads.subjects.lock().unwrap();
        if let Some((previous, all)) = &*cache {
            if *previous == sources {
                return select(all);
            }
        }
        let mut found = Subjects::new();
        for file in &messages {
            if let Some(doc) = message_file(file, self.scope.uid) {
                for (key, subject) in crate::message_subjects::index(&doc) {
                    if self.scope.precache || keys.contains(&key) {
                        found
                            .entry(key)
                            .and_modify(|old| old.merge(&subject))
                            .or_insert(subject);
                    }
                }
            }
        }
        if !found.is_empty() {
            let archive = comment_projection(&self.scope.root, self.scope.uid);
            crate::message_subjects::complete_from_comments(&mut found, &archive);
        }
        let selected = select(&found);
        *cache = (self.scope.precache
            && serde_json::to_vec(&found).is_ok_and(|bytes| bytes.len() <= MAX_CACHED_LIST))
        .then_some((sources, found));
        selected
    }

    pub fn attachment(&self, key: &str) -> Option<PathBuf> {
        if key.len() != 32 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let root = messages::base(&self.scope.root, self.scope.uid);
        self.message_documents("all", "").find_map(|doc| {
            messages::attachment_file(
                &messages::Store {
                    doc,
                    root: root.clone(),
                    notice: String::new(),
                },
                key,
            )
        })
    }

    pub fn messages(&self, ty: &str, session: &str) -> MessageSnapshot {
        let scope = &self.scope;
        let files = message_sources(scope, ty, session);
        if let Some((previous, snapshot)) = &*self.reads.messages.lock().unwrap() {
            if *previous == files {
                return snapshot.clone();
            }
        }
        let mut doc = if scope.uid == 0 {
            messages::Doc::default()
        } else {
            messages::Doc::new(scope.uid)
        };
        if !files.is_empty() {
            let _ = doc.merge_many(
                files
                    .iter()
                    .filter_map(|file| message_file(file, scope.uid)),
            );
        }
        let store = Arc::new(Mutex::new(messages::Store {
            doc,
            root: messages::base(&scope.root, scope.uid),
            notice: String::new(),
        }));
        if scope.precache
            && ty != "all"
            && files.iter().map(|f| f.size).sum::<u64>() <= MAX_CACHED_SOURCE
        {
            *self.reads.messages.lock().unwrap() = Some((files, store.clone()));
        }
        store
    }
}

pub struct Disk {
    view: RwLock<Arc<View>>,
    active: AtomicBool,
}

impl Disk {
    pub fn new(scope: Scope) -> Self {
        Self {
            view: RwLock::new(Arc::new(View::new(scope))),
            active: AtomicBool::new(false),
        }
    }
    pub fn view(&self) -> Arc<View> {
        self.view.read().unwrap().clone()
    }
    pub fn is_current(&self, view: &Arc<View>) -> bool {
        Arc::ptr_eq(&self.view(), view)
    }
    pub fn set_scope(&self, scope: Scope) {
        *self.view.write().unwrap() = Arc::new(View::new(scope));
    }
    pub fn warm(self: &Arc<Self>) {
        if !self.view().scope.precache || self.active.swap(true, Ordering::AcqRel) {
            return;
        }
        let disk = self.clone();
        std::thread::spawn(move || {
            let view = disk.view();
            if view.scope.precache {
                let _ = view.list();
            }
            disk.active.store(false, Ordering::Release);
            if !disk.is_current(&view) {
                disk.warm();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Comment, Video};

    #[test]
    fn list_is_lightweight_and_reads_desktop_updates_in_either_mode() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let mut video = Video {
            aid: 116644617717979,
            owner_mid: 12,
            r#type: "video".into(),
            comments: vec![Comment {
                rpid: 1,
                content: "initial".into(),
                ctime: 1700000000,
                ..Default::default()
            }],
            ..Default::default()
        };
        crate::export::export_item_for(root, &video, 12).unwrap();
        for precache in [true, false] {
            let view = View::new(Scope {
                precache,
                ..desktop_scope(root, 12)
            });
            let list = view.list();
            assert_eq!(list["videos"][0]["aid"], "116644617717979");
            assert_eq!(list["videos"][0]["fetched"], video.comments.len());
            let summary = view.post_summaries();
            assert_eq!(summary["videos"][0]["aid"], video.aid);
            assert_eq!(summary["videos"][0]["replies"], video.total_replies());
            assert!(summary["videos"][0].get("comments").is_none());
            assert!(list["videos"][0]["comments"].as_array().unwrap().is_empty());
            let old_count = video.comments.len();
            video.comments.push(Comment {
                rpid: old_count as u64 + 1,
                content: "second".into(),
                ..Default::default()
            });
            crate::export::export_item_for(root, &video, 12).unwrap();
            assert_eq!(view.list()["videos"][0]["fetched"], old_count + 1);
            assert_eq!(
                view.comments(Some(video.aid)).videos[0].comments.len(),
                old_count + 1
            );
            assert_eq!(view.search("second"), vec![format!("video-{}", video.aid)]);
        }
        assert!(View::new(desktop_scope(root, 13))
            .comments(None)
            .videos
            .is_empty());
    }
}
