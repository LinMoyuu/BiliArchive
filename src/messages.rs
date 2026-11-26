//! Account message archives. No mutation endpoints are used.
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

#[path = "messages_paging.rs"]
mod paging;
pub use paging::ng_count;
#[path = "messages_likes.rs"]
mod likes;
#[path = "messages_run.rs"]
mod run;
pub use likes::{item_summary, status as like_status, users as like_users};
pub use likes::{LikeDetail, LikeStatus, LikeUser};
use run::CrawlContext;
pub use run::{CrawlEvent, RunOutcome};

pub const TYPES: [&str; 4] = ["private", "likes", "replies", "mentions"];
// Stored types stay unchanged; groups is a collection/view category.
pub const VIEW_TYPES: [&str; 5] = ["private", "groups", "likes", "replies", "mentions"];
pub fn session_type(session: &Session) -> &'static str {
    if session.session_type == 2 {
        "groups"
    } else {
        "private"
    }
}
pub fn item_type(item: &Item) -> &str {
    // Session keys encode the validated session_type (user_/group_).
    if item.ty == "private" && item.session.starts_with("group_") {
        "groups"
    } else {
        &item.ty
    }
}
pub fn label(ty: &str) -> &str {
    match ty {
        "private" => "私信",
        "groups" => "粉丝团",
        "likes" => "收到的赞",
        "replies" => "回复我的",
        "mentions" => "@我的",
        _ => "全部消息",
    }
}
fn id(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}
fn num(v: &Value) -> u64 {
    v.as_u64().or_else(|| v.as_str()?.parse().ok()).unwrap_or(0)
}
fn yes(v: &Value) -> bool {
    v.as_bool().unwrap_or_else(|| num(v) > 0)
}
fn text(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}
fn first(values: &[&Value]) -> String {
    values
        .iter()
        .map(|v| text(v))
        .find(|s| !s.is_empty())
        .unwrap_or_default()
}
fn lossless(v: &mut Value) {
    match v {
        Value::Array(a) => {
            for x in a {
                lossless(x)
            }
        }
        Value::Object(m) => {
            for x in m.values_mut() {
                lossless(x)
            }
        }
        Value::Number(n) if n.as_u64().is_some_and(|n| n > 9_007_199_254_740_991) => {
            *v = Value::String(n.to_string())
        }
        _ => {}
    }
}
fn digest(value: &str) -> String {
    use md5::{Digest, Md5};
    format!("{:x}", Md5::digest(value.as_bytes()))
}
fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

#[derive(Clone, Default, Serialize, Deserialize, Debug)]
pub struct Attachment {
    pub id: String,
    pub url: String,
    pub path: String,
    pub error: String,
    #[serde(default)]
    pub mime: String,
}
#[derive(Clone, Default, Serialize, Deserialize, Debug)]
pub struct Item {
    pub id: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub session: String,
    pub sender_uid: String,
    pub sender_name: String,
    pub text: String,
    pub timestamp: u64,
    pub url: String,
    pub status: u32,
    pub attachments: Vec<Attachment>,
    #[serde(default)]
    pub like_count: u64,
    #[serde(default)]
    pub like_users: Vec<LikeUser>,
    #[serde(default)]
    pub like_detail: LikeDetail,
    pub raw: Value,
}
#[derive(Clone, Default, Serialize, Deserialize, Debug)]
pub struct Session {
    pub key: String,
    pub talker_id: String,
    pub session_type: u32,
    pub name: String,
    pub timestamp: u64,
    pub raw: Value,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Doc {
    #[serde(skip)]
    pub archive: serde_json::Value,
    pub format: String,
    pub version: u32,
    pub account_uid: String,
    pub exported_at: String,
    pub sessions: Vec<Session>,
    pub items: Vec<Item>,
}
impl Default for Doc {
    fn default() -> Self {
        Self {
            archive: serde_json::Value::Null,
            format: "bili-account-messages".into(),
            version: 1,
            account_uid: String::new(),
            exported_at: String::new(),
            sessions: vec![],
            items: vec![],
        }
    }
}
impl Doc {
    pub fn new(uid: u64) -> Self {
        Self {
            account_uid: uid.to_string(),
            ..Self::default()
        }
    }
    pub fn validate(&self, uid: u64) -> Result<()> {
        if self.format != "bili-account-messages" || self.version != 1 {
            bail!("无法识别的消息归档格式")
        }
        if self.account_uid != uid.to_string() || uid == 0 {
            bail!("消息归档账号与当前账号不匹配")
        }
        let mut keys = BTreeSet::new();
        for s in &self.sessions {
            let prefix = match s.session_type {
                1 => "user",
                2 => "group",
                _ => bail!("未知会话类型"),
            };
            if !valid_id(&s.talker_id)
                || s.key != format!("{}_{}", prefix, s.talker_id)
                || num(&s.raw["system_msg_type"]) > 0
            {
                bail!("无效或系统会话")
            }
            if !keys.insert(s.key.clone()) {
                bail!("归档中存在重复会话")
            }
        }
        for i in &self.items {
            if !TYPES.contains(&i.ty.as_str()) || i.id.is_empty() {
                bail!("无效消息类型或标识")
            }
            if i.ty == "private" && !keys.contains(&i.session) {
                bail!("私信缺少会话信息")
            }
            for a in &i.attachments {
                if a.id != digest(&a.url) {
                    bail!("附件标识不匹配")
                }
                if !a.path.is_empty() && !safe_relative(&a.path) {
                    bail!("无效附件相对路径")
                }
            }
        }
        Ok(())
    }
    pub fn merge(&mut self, other: Doc) -> Result<()> {
        self.merge_many([other])
    }

    pub fn merge_many(&mut self, docs: impl IntoIterator<Item = Doc>) -> Result<()> {
        let uid = self.account_uid.parse()?;
        let mut sessions: BTreeMap<String, usize> = self.sessions.iter().enumerate()
            .map(|(position, session)| (session.key.clone(), position)).collect();
        let mut index: BTreeMap<(String, String, String), usize> = self
            .items
            .iter()
            .enumerate()
            .map(|(n, i)| ((i.ty.clone(), i.session.clone(), i.id.clone()), n))
            .collect();
        for other in docs {
            other.validate(uid)?;
            crate::archive::merge_metadata(&mut self.archive, &other.archive);
            for session in other.sessions {
                if let Some(&position) = sessions.get(&session.key) {
                    self.sessions[position] = session;
                } else {
                    sessions.insert(session.key.clone(), self.sessions.len());
                    self.sessions.push(session);
                }
            }
            for mut i in other.items {
                if i.ty == "likes" && i.like_count == 0 {
                    i.like_count = num(&i.raw["counts"]).max(num(&i.raw["count"]));
                }
                let key = (i.ty.clone(), i.session.clone(), i.id.clone());
                if let Some(&n) = index.get(&key) {
                    if i.ty == "likes" {
                        let summary_only =
                            i.like_detail.status == LikeStatus::Pending && i.like_users.is_empty();
                        let mut users = self.items[n].like_users.clone();
                        likes::merge_users(&mut users, std::mem::take(&mut i.like_users));
                        i.like_users = users;
                        if summary_only {
                            i.like_detail = self.items[n].like_detail.clone();
                            if i.like_count != self.items[n].like_count {
                                i.like_detail = LikeDetail::default();
                            }
                        }
                    }
                    for a in &mut i.attachments {
                        if let Some(old) = self.items[n]
                            .attachments
                            .iter()
                            .find(|x| x.id == a.id && !x.path.is_empty())
                        {
                            a.path = old.path.clone();
                            a.error = old.error.clone();
                        }
                    }
                    self.items[n] = i;
                } else {
                    index.insert(key, self.items.len());
                    self.items.push(i)
                }
            }
        }
        self.exported_at = crate::model::now_text();
        Ok(())
    }
    pub fn counts(&self) -> BTreeMap<&'static str, usize> {
        VIEW_TYPES
            .iter()
            .map(|t| (*t, self.items.iter().filter(|i| item_type(i) == *t).count()))
            .collect()
    }
    pub fn summary(&self) -> Value {
        let counts = self.counts();
        let (saved, pending) = crate::archive::message_resource_counts(self);
        json!({"account_uid":self.account_uid,"counts":counts,"sessions":session_summaries(self),"total":self.items.len(),"attachments_saved":saved,"attachments_pending":pending})
    }
    /// Count ranking is local to like notices and happens before transport pagination.
    pub fn query_ordered(&self, ty: &str, session: &str, q: &str, order: &str) -> Vec<&Item> {
        let mut items = self.query(ty, session, q, order == "old");
        if ty == "likes" && matches!(order, "likes_desc" | "likes_asc") {
            // query() has already ordered equal counts by newest known time, stably.
            items.sort_by(|a, b| {
                if order == "likes_asc" {
                    a.like_count.cmp(&b.like_count)
                } else {
                    b.like_count.cmp(&a.like_count)
                }
            });
        }
        items
    }
    pub fn query(&self, ty: &str, session: &str, q: &str, ascending: bool) -> Vec<&Item> {
        let q = q.to_lowercase();
        let mut list: Vec<_> = self
            .items
            .iter()
            .filter(|i| {
                (ty == "all" || item_type(i) == ty)
                    && (session.is_empty() || i.session == session)
                    && (q.is_empty()
                        || [&i.text, &i.sender_name, &i.sender_uid]
                            .iter()
                            .any(|s| s.to_lowercase().contains(&q))
                        || i.like_users
                            .iter()
                            .any(|u| u.nickname.to_lowercase().contains(&q) || u.uid.contains(&q)))
            })
            .collect();
        list.sort_by(|a, b| match (a.timestamp, b.timestamp) {
            (0, 0) => std::cmp::Ordering::Equal,
            (0, _) => std::cmp::Ordering::Greater,
            (_, 0) => std::cmp::Ordering::Less,
            _ => {
                if ascending {
                    a.timestamp.cmp(&b.timestamp)
                } else {
                    b.timestamp.cmp(&a.timestamp)
                }
            }
        });
        list
    }
}
#[derive(Default)]
pub struct Store {
    pub doc: Doc,
    pub root: PathBuf,
    pub notice: String,
}
pub fn base(root: &Path, uid: u64) -> PathBuf {
    crate::archive::root(root)
        .join(uid.to_string())
        .join("messages")
}
pub fn safe_relative(path: &str) -> bool {
    let p = Path::new(path);
    (path.starts_with("attachments/") || path.starts_with("resources/"))
        && !path.contains('\\')
        && p.components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
        && (p.components().count() == 2 || p.components().count() == 3)
}
pub fn attachment_file(store: &Store, attachment_id: &str) -> Option<PathBuf> {
    let a = store
        .doc
        .items
        .iter()
        .flat_map(|i| i.attachments.iter())
        .find(|a| a.id == attachment_id && !a.path.is_empty())?;
    let root = crate::archive::storage_root(&store.root).canonicalize().ok()?;
    local_attachment_path(&root, &store.doc.account_uid, &a.path)
}

fn local_attachment_path(root: &Path, uid: &str, relative: &str) -> Option<PathBuf> {
    if !safe_relative(relative) {
        return None;
    }
    let path = root.join(relative).canonicalize().ok()?;
    (path.starts_with(root.join("resources").join(uid)) || path.starts_with(root.join("attachments")))
        .then_some(path)
        .filter(|path| path.is_file())
}

/// Keep lightweight attachment references so cached summaries can check the files again.
#[derive(Default)]
pub(crate) struct AttachmentInventory {
    by_type: BTreeMap<&'static str, BTreeSet<String>>,
    paths: BTreeMap<String, BTreeSet<String>>,
}

impl AttachmentInventory {
    pub fn add(&mut self, doc: &Doc) {
        for item in &doc.items {
            let ty = match item_type(item) {
                "private" | "groups" => "private",
                "replies" => "replies",
                "mentions" => "mentions",
                "likes" => "likes",
                _ => continue,
            };
            for attachment in &item.attachments {
                if attachment.id.is_empty() || crate::archive::attachment_type(item, attachment) == "cover" {
                    continue;
                }
                self.by_type.entry(ty).or_default().insert(attachment.id.clone());
                let paths = self.paths.entry(attachment.id.clone()).or_default();
                if !attachment.path.is_empty() {
                    paths.insert(attachment.path.clone());
                }
                if let Some(path) = doc.archive["resources"][&attachment.id]["path"].as_str().filter(|p| !p.is_empty()) {
                    paths.insert(path.to_owned());
                }
            }
        }
    }

    pub fn cache_bytes(&self) -> usize {
        let identifiers: usize = self.by_type.values().flatten().map(|id| id.len() + 64).sum();
        let files: usize = self.paths.iter().map(|(id, paths)| {
            id.len() + 64 + paths.iter().map(|path| path.len() + 64).sum::<usize>()
        }).sum();
        identifiers + files
    }

    pub fn counts(&self, root: &Path, uid: &str) -> Value {
        let root = root.canonicalize().ok();
        let saved: BTreeSet<_> = self.paths.iter().filter(|(_, paths)| {
            root.as_ref().is_some_and(|root| paths.iter().any(|relative| {
                local_attachment_path(root, uid, relative).is_some_and(|path| {
                    std::fs::metadata(path).is_ok_and(|meta| meta.len() > 0)
                })
            }))
        }).map(|(id, _)| id.as_str()).collect();
        let mut totals = serde_json::Map::new();
        for ty in ["replies", "mentions", "likes", "private"] {
            let ids = self.by_type.get(ty);
            let count = ids.map_or(0, BTreeSet::len);
            let saved = ids.into_iter().flatten().filter(|id| saved.contains(id.as_str())).count();
            totals.insert(ty.into(), json!({"saved": saved, "pending": count - saved}));
        }
        Value::Object(totals)
    }
}
pub fn load(root: &Path, uid: u64) -> Result<Store> {
    load_with_workers(root, uid, crate::local_read::workers())
}
pub fn load_with_workers(root: &Path, uid: u64, workers: usize) -> Result<Store> {
    load_internal(root, uid, workers, false)
}

pub fn load_for_import(root: &Path, uid: u64, workers: usize) -> Result<Store> {
    load_internal(root, uid, workers, true)
}

fn load_internal(root: &Path, uid: u64, workers: usize, require_valid_files: bool) -> Result<Store> {
    let dir = base(root, uid);
    let mut doc = Doc::new(uid);
    let mut errors = Vec::new();
    if root.join(uid.to_string()).join("messages").is_dir() {
        errors.push("发现不受支持的旧版消息归档，请重新采集；原文件未改动".into());
    }
    if crate::checkpoint::root_dir()?
        .join("messages")
        .join(format!("{uid}.json"))
        .exists()
    {
        errors.push("旧消息断点与新版归档不兼容，原文件保留；重新采集将使用独立的 v2 断点".into());
    }
    let mut paths: Vec<PathBuf> = TYPES
        .iter()
        .filter(|t| **t != "private")
        .map(|t| dir.join(format!("{}.json", t)))
        .filter(|p| p.exists())
        .collect();
    if dir.join("private").exists() {
        for p in std::fs::read_dir(dir.join("private"))? {
            let p = p?.path();
            if p.extension().is_some_and(|e| e == "json") {
                paths.push(p)
            }
        }
    }
    // Read independent files in bounded batches, then merge in source order.
    let mut documents = Vec::with_capacity(paths.len());
    for batch in paths.chunks(32) {
        let loaded = crate::local_read::map_ordered(batch, workers, |path| -> Result<Doc> {
            let doc = crate::archive::messages(&crate::archive::Envelope::parse(&std::fs::read(path)?)?)?;
            doc.validate(uid)?;
            Ok(doc)
        });
        for (path, result) in batch.iter().zip(loaded) {
            match result {
                Ok(doc) => documents.push(doc),
                Err(error) if require_valid_files => {
                    return Err(error.context(format!("现有消息归档损坏，未覆盖：{}", path.display())));
                }
                Err(error) => errors.push(format!("{}：{}", path.display(), error)),
            }
        }
    }
    if !documents.is_empty() { doc.merge_many(documents)?; }
    // A copied JSON may refer to attachments not present at the new location.
    let resource_root = crate::archive::storage_root(&dir);
    let mut attachment_files = BTreeMap::new();
    for a in doc.items.iter_mut().flat_map(|i| i.attachments.iter_mut()) {
        if !a.path.is_empty() && !*attachment_files.entry(a.path.clone())
            .or_insert_with(|| attachment_exists(&resource_root, &a.path)) {
            a.path.clear();
            a.error = "本地附件缺失，待补下载".into()
        }
    }
    let saved: BTreeMap<_, _> = doc
        .items
        .iter()
        .flat_map(|i| &i.attachments)
        .filter(|a| !a.path.is_empty())
        .map(|a| (a.id.clone(), a.clone()))
        .collect();
    for a in doc.items.iter_mut().flat_map(|i| &mut i.attachments) {
        if a.path.is_empty() {
            if let Some(known) = saved.get(&a.id) {
                a.path = known.path.clone();
                a.mime = known.mime.clone();
                a.error.clear();
            }
        }
    }
    Ok(Store {
        doc,
        root: dir,
        notice: errors.join("\n"),
    })
}
fn write_doc(path: &Path, doc: &Doc) -> Result<()> {
    let root = crate::archive::storage_root(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    );
    let mut scoped = doc.clone();
    scoped.archive["export_scope"] = json!(path.file_stem().unwrap_or_default().to_string_lossy());
    let archive =
        crate::archive::from_messages(&scoped, &root, crate::archive::ResourcePolicy::load());
    crate::archive::write(path, &archive)
}

pub fn save(store: &Store) -> Result<()> {
    let groups = TYPES.iter().filter(|ty| **ty != "private").map(|ty| ty.to_string())
        .chain(store.doc.sessions.iter().map(|session|session.key.clone())).collect();
    save_groups(store, &groups)
}

pub fn save_groups(store: &Store, groups: &BTreeSet<String>) -> Result<()> {
    let uid = store.doc.account_uid.parse()?;
    store.doc.validate(uid)?;
    let sessions: BTreeMap<_, _> = store.doc.sessions.iter().map(|session|(session.key.as_str(), session)).collect();
    let mut items: BTreeMap<&str, Vec<Item>> = BTreeMap::new();
    for item in &store.doc.items {
        let key = if item.ty == "private" { &item.session } else { &item.ty };
        if groups.contains(key) {
            items.entry(key.as_str()).or_default().push(item.clone());
        }
    }
    for group in groups {
        let mut doc = Doc::new(uid);
        doc.archive = store.doc.archive.clone();
        doc.exported_at = store.doc.exported_at.clone();
        doc.items = items.remove(group.as_str()).unwrap_or_default();
        let path = if TYPES.contains(&group.as_str()) {
            store.root.join(format!("{group}.json"))
        } else {
            let session = sessions.get(group.as_str()).context("会话缺失")?;
            doc.sessions.push((**session).clone());
            store.root.join("private").join(format!("{group}.json"))
        };
        write_doc(&path, &doc)?;
    }
    Ok(())
}

pub fn export_bundle(store: &Store, path: &Path) -> Result<()> {
    if path.starts_with(store.root.join("private")) || path.parent() == Some(store.root.as_path()) {
        bail!("请将合并导出保存到归档目录之外，避免覆盖分类文件")
    }
    let root = crate::archive::storage_root(&store.root);
    let mut archive =
        crate::archive::from_messages(&store.doc, &root, crate::archive::ResourcePolicy::load());
    crate::archive::copy_resources(
        &mut archive,
        &root,
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?;
    crate::archive::write(path, &archive)
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct Options {
    pub types: Vec<String>,
    pub from: u64,
    pub to: u64,
}
impl Options {
    pub fn parse(types: Vec<String>, from: &str, to: &str) -> Result<Self> {
        if types.is_empty() || types.iter().any(|t| !VIEW_TYPES.contains(&t.as_str())) {
            bail!("请至少选择一种消息类型")
        }
        let (from, to) = crate::message_dates::bounds(from, to)?;
        Ok(Self { types, from, to })
    }
    fn session_enabled(&self, session: &Session) -> bool {
        self.types.iter().any(|t| t == session_type(session))
    }
    fn item_enabled(&self, item: &Item) -> bool {
        self.types.iter().any(|t| t == item_type(item)) && self.contains(item.timestamp)
    }
    fn session_categories(&self) -> Vec<u32> {
        let mut result = vec![];
        if self.types.iter().any(|t| t == "private") {
            result.extend([1, 2, 5, 6]);
        }
        if self.types.iter().any(|t| t == "groups") {
            result.push(3);
        }
        result
    }
    fn contains(&self, t: u64) -> bool {
        (self.from == 0 || t >= self.from) && (self.to == 0 || t <= self.to)
    }
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct Cursor {
    id: String,
    time: u64,
    done: bool,
}
#[derive(Clone, Serialize, Deserialize)]
struct SessionTask {
    session: Session,
    seq: u64,
    done: bool,
}
#[derive(Clone, Serialize, Deserialize)]
struct Task {
    #[serde(skip)]
    path: PathBuf,
    library_id: String,
    #[serde(skip)]
    summary_bounded: bool,
    version: u32,
    uid: u64,
    options: Options,
    feeds: BTreeMap<String, Cursor>,
    lists: BTreeMap<u32, Cursor>,
    sessions: BTreeMap<String, SessionTask>,
    #[serde(default)]
    likes: BTreeMap<String, likes::LikeCursor>,
}
pub fn validate_portable_task(value: Value, uid: u64, library_id: &str) -> Result<()> {
    let task: Task = serde_json::from_value(value).context("消息断点内容无效")?;
    anyhow::ensure!(task.version == 2 && task.uid == uid && task.library_id == library_id, "消息断点归属或版本不匹配");
    Ok(())
}
pub fn validate_portable_ng(value: Value, uid: u64, library_id: &str) -> Result<()> {
    paging::validate_portable(value, uid, library_id)
}
fn task_path(uid: u64) -> Result<PathBuf> {
    let path = crate::checkpoint::root_dir()?
        .join("messages/v2")
        .join(format!("{uid}.json"));
    crate::library::migrate_message_task(uid, &path)?;
    Ok(path)
}
pub fn clear_task(uid: u64) -> Result<()> {
    let p = task_path(uid)?;
    if p.exists() {
        std::fs::remove_file(p)?
    };
    Ok(())
}
pub fn has_resumable_task(uid: u64, store: &Store) -> Result<bool> {
    paging::has_resumable_at(
        &task_path(uid)?,
        uid,
        &store.root.to_string_lossy(),
        &store.doc,
    )
}
impl Task {
    fn open(uid: u64, options: Options, path: PathBuf, archive_root: String) -> Result<Self> {
        if path.exists() {
            let mut task: Self = serde_json::from_value(
                crate::library::read_state(&path, uid, Path::new(&archive_root))?
                    .context("消息断点所属资料库不匹配")?,
            )
            .context("消息断点损坏，未覆盖")?;
            if task.version != 2 || task.uid != uid {
                bail!("消息断点版本或账号不匹配")
            }
            if task.options == options
                && task.library_id == crate::library::id_for_archive(Path::new(&archive_root))?
            {
                task.path = path;
                return Ok(task);
            }
        }
        Self::fresh(uid, options, path, archive_root)
    }
    fn fresh(uid: u64, options: Options, path: PathBuf, archive_root: String) -> Result<Self> {
        Ok(Self {
            path,
            library_id: crate::library::id_for_archive(Path::new(&archive_root))?,
            summary_bounded: false,
            version: 2,
            uid,
            options,
            feeds: BTreeMap::new(),
            lists: BTreeMap::new(),
            sessions: BTreeMap::new(),
            likes: BTreeMap::new(),
        })
    }
    fn save(&self) -> Result<()> {
        crate::secure::write_config(&self.path, &serde_json::to_value(self)?)
    }
}
fn session(v: &Value) -> Result<Option<Session>> {
    if num(&v["system_msg_type"]) > 0 {
        return Ok(None);
    }
    let ty = num(&v["session_type"]) as u32;
    if ty != 1 && ty != 2 {
        return Ok(None);
    }
    let uid = id(&v["talker_id"]);
    if !valid_id(&uid) {
        bail!("会话缺少有效 talker_id")
    }
    let key = format!("{}_{}", if ty == 2 { "group" } else { "user" }, uid);
    let mut raw = v.clone();
    lossless(&mut raw);
    Ok(Some(Session {
        key,
        talker_id: uid.clone(),
        session_type: ty,
        name: first(&[&v["group_name"], &v["account_info"]["name"], &v["name"]])
            .trim()
            .to_owned(),
        timestamp: num(&v["session_ts"]),
        raw,
    }))
}
fn collect_attachments(v: &Value, out: &mut Vec<Attachment>) {
    match v {
        Value::Array(a) => {
            for x in a {
                collect_attachments(x, out)
            }
        }
        Value::Object(m) => {
            for (key, value) in m {
                // Only explicit media fields, never arbitrary card/navigation links.
                if [
                    "url",
                    "src",
                    "img_src",
                    "image",
                    "thumb",
                    "cover",
                    "pic_url",
                    "voice_url",
                    "audio_url",
                    "file_url",
                    "image_url",
                    "imageUrl",
                    "cover_url",
                    "voice_uri",
                    "audio_uri",
                    "file_uri",
                    "image_urls",
                    "images",
                    "pictures",
                    "image_list",
                ]
                .contains(&key.as_str())
                {
                    if let Some(values) = value.as_array() {
                        for value in values {
                            if let Some(raw) = value.as_str() {
                                let url = if raw.starts_with("//") {
                                    format!("https:{}", raw)
                                } else {
                                    raw.to_owned()
                                };
                                if (url.starts_with("http://") || url.starts_with("https://"))
                                    && !out.iter().any(|a| a.url == url)
                                {
                                    out.push(Attachment {
                                        id: digest(&url),
                                        url,
                                        ..Default::default()
                                    })
                                }
                            }
                        }
                    }
                    if let Some(url) = value.as_str() {
                        let url = if url.starts_with("//") {
                            format!("https:{}", url)
                        } else {
                            url.to_owned()
                        };
                        let media = key != "url"
                            || m.contains_key("height")
                            || m.contains_key("width")
                            || m.contains_key("duration")
                            || m.contains_key("file_name")
                            || m.contains_key("imageType");
                        if media
                            && (url.starts_with("https://") || url.starts_with("http://"))
                            && !out.iter().any(|a| a.url == url)
                        {
                            out.push(Attachment {
                                id: digest(&url),
                                url,
                                ..Default::default()
                            })
                        }
                    }
                }
                if value.is_object() || value.is_array() {
                    collect_attachments(value, out)
                }
            }
        }
        _ => {}
    }
}
fn private_item(v: &Value, s: &Session) -> Result<Item> {
    let key = id(&v["msg_key"]);
    let seq = id(&v["msg_seqno"]);
    let key = if !key.is_empty() && key != "0" {
        key
    } else {
        format!("seq-{}", seq)
    };
    if seq.is_empty() {
        bail!("私信缺少序列号")
    }
    let content = match &v["content"] {
        Value::String(s) => serde_json::from_str::<Value>(s).unwrap_or(Value::String(s.clone())),
        other => other.clone(),
    };
    let body = if content.is_string() {
        text(&content)
    } else {
        first(&[
            &content["content"],
            &content["text"],
            &content["title"],
            &content["headline"],
        ])
    };
    let mut attachments = vec![];
    collect_attachments(&content, &mut attachments);
    if [2, 6].contains(&num(&v["msg_type"])) {
        if let Some(url) = content["url"].as_str() {
            let url = if url.starts_with("//") {
                format!("https:{}", url)
            } else {
                url.to_owned()
            };
            if (url.starts_with("https://") || url.starts_with("http://"))
                && !attachments.iter().any(|a| a.url == url)
            {
                attachments.push(Attachment {
                    id: digest(&url),
                    url,
                    ..Default::default()
                })
            }
        }
    }
    let mut raw = v.clone();
    lossless(&mut raw);
    Ok(Item {
        id: key,
        ty: "private".into(),
        session: s.key.clone(),
        sender_uid: id(&v["sender_uid"]),
        sender_name: first(&[&v["sender_name"], &v["uname"]]),
        text: if body.is_empty() {
            format!("[消息类型 {}]", num(&v["msg_type"]))
        } else {
            body
        },
        timestamp: num(&v["timestamp"]),
        url: String::new(),
        status: num(&v["msg_status"]) as u32,
        attachments,
        raw,
        ..Default::default()
    })
}
fn feed_item(v: &Value, ty: &str) -> Result<Item> {
    let key = id(&v["id"]);
    if key.is_empty() {
        bail!("通知缺少 ID")
    }
    let user = if v["user"].is_object() {
        &v["user"]
    } else {
        v["users"].get(0).unwrap_or(&Value::Null)
    };
    let body = [
        "title",
        "source_content",
        "target_reply_content",
        "root_reply_content",
        "desc",
        "message",
    ]
    .iter()
    .map(|k| text(&v["item"][k]))
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join("\n");
    let mut attachments = vec![];
    collect_attachments(&v["item"], &mut attachments);
    let mut raw = v.clone();
    lossless(&mut raw);
    let timestamp = ["like_time", "reply_time", "at_time", "time"]
        .iter()
        .map(|k| num(&v[k]))
        .find(|n| *n > 0)
        .unwrap_or(0);
    Ok(Item {
        like_count: if ty == "likes" {
            num(&v["counts"]).max(num(&v["count"]))
        } else {
            0
        },
        id: key,
        ty: ty.into(),
        session: String::new(),
        sender_uid: id(&user["mid"]),
        sender_name: if let Some(users) = v["users"].as_array() {
            users
                .iter()
                .map(|u| first(&[&u["nickname"], &u["uname"]]))
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("、")
        } else {
            first(&[&user["nickname"], &user["uname"]])
        },
        text: body,
        timestamp,
        url: text(&v["item"]["uri"]),
        status: 0,
        attachments,
        raw,
        ..Default::default()
    })
}
fn array<'a>(v: &'a Value, key: &str) -> Result<&'a [Value]> {
    match &v[key] {
        Value::Array(a) => Ok(a),
        Value::Null => Ok(&[]),
        _ => bail!("接口 {} 不是数组", key),
    }
}
fn data(v: &Value) -> Result<&Value> {
    if v.get("code").and_then(Value::as_i64) != Some(0) {
        bail!("消息接口错误 {}：{}", v["code"], v["message"])
    };
    if !v["data"].is_object() {
        bail!("消息接口未返回有效 data")
    };
    Ok(&v["data"])
}
fn save_group(store: &Store, group: &str) -> Result<()> {
    let uid = store.doc.account_uid.parse()?;
    let mut d = Doc::new(uid);
    d.archive = store.doc.archive.clone();
    d.exported_at = store.doc.exported_at.clone();
    let path = if TYPES.contains(&group) {
        d.items = store
            .doc
            .items
            .iter()
            .filter(|i| i.ty == group)
            .cloned()
            .collect();
        store.root.join(format!("{}.json", group))
    } else {
        let session = store
            .doc
            .sessions
            .iter()
            .find(|s| s.key == group)
            .ok_or_else(|| anyhow!("会话缺失"))?;
        d.sessions.push(session.clone());
        d.items = store
            .doc
            .items
            .iter()
            .filter(|i| i.ty == "private" && i.session == group)
            .cloned()
            .collect();
        store.root.join("private").join(format!("{}.json", group))
    };
    d.validate(uid)?;
    write_doc(&path, &d)
}
#[derive(Debug)]
struct PersistFailure;
impl std::fmt::Display for PersistFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "消息归档、断点或采集报告写入失败，已停止采集")
    }
}
impl std::error::Error for PersistFailure {}
pub fn is_persist_failure(error: &anyhow::Error) -> bool {
    error.is::<PersistFailure>()
}
fn checkpoint_store(store: &Arc<Mutex<Store>>, task: &Task, group: &str) -> Result<()> {
    let mut state = store.lock().unwrap();
    if !state.doc.archive.is_object() {
        state.doc.archive = json!({});
    }
    let mut groups: Vec<String> = task
        .options
        .types
        .iter()
        .filter(|t| t.as_str() != "private" && t.as_str() != "groups")
        .cloned()
        .collect();
    groups.extend(task.sessions.keys().cloned());
    for key in groups {
        let summary = run::file_summary(&state, task, &key);
        state.doc.archive["collections"][&key] = summary;
    }
    if group == "all" {
        save(&state).context(PersistFailure)?
    } else {
        save_group(&state, group).context(PersistFailure)?
    };
    task.save().context(PersistFailure)
}
fn merge_page(
    store: &Arc<Mutex<Store>>,
    uid: u64,
    sessions: Vec<Session>,
    items: Vec<Item>,
) -> Result<()> {
    let mut d = Doc::new(uid);
    d.sessions = sessions;
    d.items = items;
    store.lock().unwrap().doc.merge(d)
}

/// Event-based collection writes a separate report; source documents contain file summaries only.
pub fn crawl_events(
    store: &Arc<Mutex<Store>>,
    uid: u64,
    options: Options,
    cancel: &AtomicBool,
    paused: &AtomicBool,
    request: impl FnMut(&str) -> Result<Value>,
    download: impl FnMut(&str, &Path) -> Result<()>,
    emit: impl FnMut(CrawlEvent),
) -> Result<RunOutcome> {
    crawl_events_mode(
        store, uid, options, cancel, paused, request, download, emit, false,
    )
}
pub fn crawl_events_mode(
    store: &Arc<Mutex<Store>>,
    uid: u64,
    options: Options,
    cancel: &AtomicBool,
    paused: &AtomicBool,
    request: impl FnMut(&str) -> Result<Value>,
    download: impl FnMut(&str, &Path) -> Result<()>,
    mut emit: impl FnMut(CrawlEvent),
    retry_ng: bool,
) -> Result<RunOutcome> {
    let root = store.lock().unwrap().root.clone();
    let mut sleep = |duration| std::thread::sleep(duration);
    let mut ctx = CrawlContext::new(cancel, &mut emit, &mut sleep);
    let path = if retry_ng {
        task_path(uid)?.with_extension("ng.json")
    } else {
        task_path(uid)?
    };
    ctx.manual_paging = retry_ng;
    ctx.monitor(store);
    ctx.start(&root, uid, &options)?;
    let result = crawl_run(
        store,
        uid,
        options,
        cancel,
        path.clone(),
        request,
        download,
        &mut ctx,
    );
    let fatal = result
        .as_ref()
        .err()
        .is_some_and(|e| e.is::<PersistFailure>())
        || (result.is_err() && !cancel.load(Ordering::SeqCst));
    if fatal {
        ctx.issue(format!(
            "消息采集失败，进度已保留：{:#}",
            result.as_ref().err().unwrap()
        ))?;
    }
    let status = if fatal {
        "failed"
    } else if cancel.load(Ordering::SeqCst) {
        if paused.load(Ordering::SeqCst) {
            "paused"
        } else {
            "stopped"
        }
    } else if result.as_ref().is_ok_and(|e| !e.is_empty()) {
        "partial"
    } else {
        "completed"
    };
    let snapshot = store.lock().unwrap().doc.clone();
    let report = ctx.finish(status, &snapshot)?;
    if (status == "completed" || status == "stopped") && path.exists() {
        if let Err(error) = std::fs::remove_file(&path).context(PersistFailure) {
            ctx.issue(format!("清理消息断点失败：{error:#}"))?;
            ctx.finish("failed", &snapshot)?;
            return Err(error);
        }
    }
    if fatal {
        return Err(result.err().unwrap());
    }
    Ok(RunOutcome {
        paging: ctx.paging.as_ref().map(|p| p.stats).unwrap_or_default(),
        paging_unresolved: ctx.paging.as_ref().map(|p| p.unresolved()).unwrap_or(0),
        errors: result.unwrap_or_default(),
        report,
    })
}
pub fn crawl(
    store: &Arc<Mutex<Store>>,
    uid: u64,
    options: Options,
    cancel: &AtomicBool,
    request: impl FnMut(&str) -> Result<Value>,
    download: impl FnMut(&str, &Path) -> Result<()>,
    mut progress: impl FnMut(String),
) -> Result<Vec<String>> {
    let paused = AtomicBool::new(false);
    let outcome = crawl_events(
        store,
        uid,
        options,
        cancel,
        &paused,
        request,
        download,
        |event| match event {
            CrawlEvent::Stage(text)
            | CrawlEvent::Progress(text)
            | CrawlEvent::Issue(text)
            | CrawlEvent::Deferred(text) => progress(text),
            _ => {}
        },
    )?;
    Ok(outcome.errors)
}
#[cfg(test)]
fn crawl_at(
    store: &Arc<Mutex<Store>>,
    uid: u64,
    options: Options,
    cancel: &AtomicBool,
    path: PathBuf,
    request: impl FnMut(&str) -> Result<Value>,
    download: impl FnMut(&str, &Path) -> Result<()>,
    mut progress: impl FnMut(String),
) -> Result<Vec<String>> {
    let mut emit = |event| {
        if let CrawlEvent::Progress(text) = event {
            progress(text)
        }
    };
    let mut sleep = |_| {};
    let mut ctx = CrawlContext::new(cancel, &mut emit, &mut sleep);
    crawl_run(
        store, uid, options, cancel, path, request, download, &mut ctx,
    )
}
fn crawl_run(
    store: &Arc<Mutex<Store>>,
    uid: u64,
    options: Options,
    cancel: &AtomicBool,
    path: PathBuf,
    mut request: impl FnMut(&str) -> Result<Value>,
    mut download: impl FnMut(&str, &Path) -> Result<()>,
    ctx: &mut CrawlContext<'_>,
) -> Result<Vec<String>> {
    let archive_root = store.lock().unwrap().root.to_string_lossy().into_owned();
    let resumable = if ctx.manual_paging {
        true
    } else {
        let state = store.lock().unwrap();
        paging::has_resumable_at(&path, uid, &archive_root, &state.doc)?
    };
    let mut task = Task::open(uid, options.clone(), path.clone(), archive_root.clone())?;
    if !resumable {
        task = Task::fresh(uid, options.clone(), path, archive_root.clone())?;
    }
    task.save().context(PersistFailure)?;
    ctx.paging = Some(paging::Paging::open(
        &task.path,
        uid,
        &archive_root,
        ctx.manual_paging,
    )?);
    let mut errors = Vec::new();
    if !ctx.manual_paging {
        errors.extend(crawl_pass(
            store,
            uid,
            &options,
            cancel,
            &mut task,
            &mut request,
            ctx,
        )?);
    }
    let mut retry_index = 0;
    while !cancel.load(Ordering::SeqCst) {
        let next = ctx.paging.as_mut().unwrap().next()?;
        let Some((_key, job)) = next else { break };
        retry_index += 1;
        ctx.paging_retry_progress(retry_index, &job)?;
        job.position.apply(&mut task);
        errors.extend(crawl_pass(
            store,
            uid,
            &job.options,
            cancel,
            &mut task,
            &mut request,
            ctx,
        )?);
    }
    if cancel.load(Ordering::SeqCst) {
        ctx.paging.as_mut().unwrap().suspend(&task)?;
    }
    let paging = ctx.paging.as_ref().unwrap();
    if paging.unresolved() > 0 {
        errors.push(format!(
            "分页异常尚未完成 {} 项；NG 可使用「重试NG项」补采",
            paging.unresolved()
        ));
    }
    if cancel.load(Ordering::SeqCst) {
        task.save().context(PersistFailure)?;
        return Ok(errors);
    }
    if ctx.manual_paging {
        checkpoint_store(store, &task, "all")?;
        return Ok(errors);
    }
    ctx.progress("处理消息附件".into(), "attachments");
    let mut attachment_errors = 0;
    let report = process_configured_attachments(
        store,
        uid,
        cancel,
        |item| options.item_enabled(item),
        &mut download,
        |report, key| {
            for error in report.errors.iter().skip(attachment_errors) {
                ctx.resource_issue(key, error.clone())?;
            }
            attachment_errors = report.errors.len();
            ctx.update(format!(
                "附件 {}/{} · 成功 {} / 失败 {} · {}",
                report.processed, report.total, report.saved, report.failed, key
            ));
            Ok(())
        },
    )?;
    errors.extend(report.errors);
    checkpoint_store(store, &task, "all")?;
    if !ctx.live && !cancel.load(Ordering::SeqCst) && errors.is_empty() && task.path.exists() {
        std::fs::remove_file(&task.path)?;
    }
    Ok(errors)
}
fn crawl_pass(
    store: &Arc<Mutex<Store>>,
    uid: u64,
    options: &Options,
    cancel: &AtomicBool,
    task: &mut Task,
    request: &mut impl FnMut(&str) -> Result<Value>,
    ctx: &mut CrawlContext<'_>,
) -> Result<Vec<String>> {
    task.summary_bounded |= options.from > 0 || options.to > 0;
    let mut errors = Vec::new();
    for ty in options
        .types
        .iter()
        .filter(|t| t.as_str() != "private" && t.as_str() != "groups")
    {
        if !ctx.paging_allows(&paging::Target::Feed(ty.clone())) {
            continue;
        }
        let mut cursor = task.feeds.get(ty).cloned().unwrap_or_default();
        let result = (|| -> Result<()> {
            let endpoint = match ty.as_str() {
                "likes" => "like",
                "replies" => "reply",
                _ => "at",
            };
            let time_key = match ty.as_str() {
                "likes" => "like_time",
                "replies" => "reply_time",
                _ => "at_time",
            };
            let mut seen = BTreeSet::new();
            while !cursor.done && !cancel.load(Ordering::SeqCst) {
                if !ctx.paging_begin(paging::Position::Feed(ty.clone(), cursor.clone()), options)? {
                    break;
                }
                ctx.progress(
                    format!(
                        "{} · 读取游标 {}",
                        label(ty),
                        if cursor.id.is_empty() {
                            "首页"
                        } else {
                            &cursor.id
                        }
                    ),
                    ty,
                );
                let url=format!("https://api.bilibili.com/x/msgfeed/{}?platform=web&mobi_app=web&build=0&id={}&{}={}",endpoint,urlencoding::encode(&cursor.id),time_key,cursor.time);
                let response = ctx.request(request, &url)?;
                let d = data(&response)?;
                let body = if ty == "likes" { &d["total"] } else { d };
                let mut raw = array(body, "items")?.to_vec();
                if ty == "likes" && cursor.id.is_empty() {
                    raw.extend_from_slice(array(&d["latest"], "items")?);
                }
                let mut items = vec![];
                for v in &raw {
                    let i = feed_item(v, ty)?;
                    if options.contains(i.timestamp) {
                        items.push(i)
                    }
                }
                merge_page(store, uid, vec![], items)?;
                if !body["cursor"].is_object()
                    || !(body["cursor"]["is_end"].is_boolean() || body["cursor"]["is_end"].is_u64())
                {
                    checkpoint_store(store, task, ty)?;
                    ctx.paging_failed(
                        "通知缺少有效分页信息".into(),
                        json!({"reason":"invalid_paging"}),
                    )?;
                    break;
                }
                let next = &body["cursor"];
                let next_id = id(&next["id"]);
                let next_time = num(&next["time"]);
                let end = yes(&next["is_end"])
                    || (options.from > 0 && next_time > 0 && next_time < options.from);
                if !end
                    && (raw.is_empty()
                        || next_id.is_empty()
                        || (next_id == cursor.id && next_time == cursor.time)
                        || seen.contains(&(next_id.clone(), next_time)))
                {
                    save(&store.lock().unwrap()).context(PersistFailure)?;
                    ctx.paging_failed("分页未前进，已保留消息与断点".into(),json!({"reason":"non_advancing_cursor","cursor_id":cursor.id,"cursor_time":cursor.time}))?;
                    break;
                }
                seen.insert((next_id.clone(), next_time));
                cursor = Cursor {
                    id: next_id,
                    time: next_time,
                    done: end,
                };
                task.feeds.insert(ty.clone(), cursor.clone());
                checkpoint_store(store, task, ty)?;
                ctx.paging_accepted()?;
                ctx.recovered()?;
                let count = store
                    .lock()
                    .unwrap()
                    .doc
                    .items
                    .iter()
                    .filter(|i| i.ty == *ty)
                    .count();
                ctx.update(format!(
                    "{} · 本页读取 {} 条 · 已归档 {} 条{}",
                    label(ty),
                    raw.len(),
                    count,
                    if end { " · 遍历结束" } else { "" }
                ));
            }
            Ok(())
        })();
        if let Err(e) = result {
            if e.is::<PersistFailure>() || e.to_string().contains("-101") {
                return Err(e);
            }
            if cancel.load(Ordering::SeqCst) {
                break;
            }
            if ctx.live {
                return Err(e);
            }
            ctx.issue(format!("{}：{}", label(ty), e))?;
            errors.push(format!("{}：{}", label(ty), e));
        }
    }
    if options.types.iter().any(|t| t == "likes") && !cancel.load(Ordering::SeqCst) {
        errors.extend(likes::collect(store, &options, task, cancel, request, ctx)?);
    }
    if options
        .types
        .iter()
        .any(|t| t == "private" || t == "groups")
    {
        // All-session mode can ignore end_ts. Walk the disjoint pageable categories.
        for category in options.session_categories() {
            if !ctx.paging_allows(&paging::Target::List(category)) {
                continue;
            }
            let mut cursor = task.lists.get(&category).cloned().unwrap_or_default();
            let mut seen = BTreeSet::new();
            let result = (|| -> Result<()> {
                while !cursor.done && !cancel.load(Ordering::SeqCst) {
                    if !ctx
                        .paging_begin(paging::Position::List(category, cursor.clone()), options)?
                    {
                        break;
                    }
                    ctx.progress(
                        format!("私信 · 读取会话分类 {}", category),
                        &category.to_string(),
                    );
                    let url=format!("https://api.vc.bilibili.com/session_svr/v1/session_svr/get_sessions?session_type={}&group_fold=0&unfollow_fold=0&sort_rule=2&size=100&mobi_app=web{}",category,if cursor.time>0{format!("&end_ts={}",cursor.time)}else{String::new()});
                    let response = ctx.request(request, &url)?;
                    let d = data(&response)?;
                    let rows = array(d, "session_list")?;
                    let mut sessions = vec![];
                    for row in rows {
                        if let Some(s) = session(row)? {
                            if !options.session_enabled(&s) {
                                continue;
                            }
                            task.sessions.entry(s.key.clone()).or_insert(SessionTask {
                                session: s.clone(),
                                seq: 0,
                                done: false,
                            });
                            sessions.push(s);
                        }
                    }
                    merge_page(store, uid, sessions, vec![])?;
                    if !(d["has_more"].is_boolean() || d["has_more"].is_u64()) {
                        checkpoint_store(store, task, "all")?;
                        ctx.paging_failed(
                            "缺少有效 has_more 分页信息".into(),
                            json!({"reason":"invalid_paging"}),
                        )?;
                        break;
                    }
                    let next = rows
                        .iter()
                        .map(|r| num(&r["session_ts"]))
                        .filter(|t| *t > 0)
                        .min()
                        .unwrap_or(0);
                    let end = !yes(&d["has_more"]);
                    if !end
                        && (rows.is_empty()
                            || next == 0
                            || (cursor.time > 0 && next >= cursor.time)
                            || seen.contains(&next))
                    {
                        checkpoint_store(store, task, "all")?;
                        ctx.paging_failed(
                            "会话游标未前进，未将该分类标为完成".into(),
                            json!({"reason":"non_advancing_cursor","cursor_time":cursor.time}),
                        )?;
                        break;
                    }
                    seen.insert(next);
                    cursor = Cursor {
                        time: next,
                        done: end,
                        ..Default::default()
                    };
                    task.lists.insert(category, cursor.clone());
                    checkpoint_store(store, task, "all")?;
                    ctx.paging_accepted()?;
                    ctx.recovered()?;
                }
                Ok(())
            })();
            if let Err(e) = result {
                if e.is::<PersistFailure>() || e.to_string().contains("-101") {
                    return Err(e);
                }
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                if ctx.live {
                    return Err(e);
                }
                ctx.issue(format!("私信会话分类 {}：{}", category, e))?;
                errors.push(format!("私信会话分类 {}：{}", category, e));
            }
        }
        let keys: Vec<_> = task
            .sessions
            .iter()
            .filter(|(_, st)| options.session_enabled(&st.session))
            .map(|(key, _)| key.clone())
            .collect();
        let total = keys.len();
        for (index, key) in keys.into_iter().enumerate() {
            if !ctx.paging_allows(&paging::Target::Session(key.clone())) {
                continue;
            }
            let mut st = task.sessions[&key].clone();
            let mut seen = BTreeSet::new();
            let result = (|| -> Result<()> {
                if !st.done
                    && !ctx
                        .paging_begin(paging::Position::Session(key.clone(), st.clone()), options)?
                {
                    return Ok(());
                }

                if !st.done
                    && st.seq == 0
                    && st.session.session_type == 1
                    && st.session.name.is_empty()
                    && !cancel.load(Ordering::SeqCst)
                {
                    ctx.progress(format!("读取私信会话资料 · {}", key), &key);
                    let profile = ctx.request(
                        request,
                        &format!(
                            "https://api.bilibili.com/x/web-interface/card?mid={}",
                            st.session.talker_id
                        ),
                    );
                    if ctx.live && profile.is_err() && !cancel.load(Ordering::SeqCst) {
                        return Err(profile.err().unwrap());
                    }
                    if let Ok(response) = profile {
                        if let Ok(d) = data(&response) {
                            st.session.name = text(&d["card"]["name"]);
                        }
                    }
                    ctx.recovered()?;
                    task.sessions.insert(key.clone(), st.clone());
                    merge_page(store, uid, vec![st.session.clone()], vec![])?;
                }
                while !st.done && !cancel.load(Ordering::SeqCst) {
                    if !ctx
                        .paging_begin(paging::Position::Session(key.clone(), st.clone()), options)?
                    {
                        break;
                    }
                    ctx.progress(
                        format!("私信 {}/{} · {} · 序列 {}", index + 1, total, key, st.seq),
                        &key,
                    );
                    let url=format!("https://api.vc.bilibili.com/svr_sync/v1/svr_sync/fetch_session_msgs?talker_id={}&session_type={}&size=100&mobi_app=web{}",st.session.talker_id,st.session.session_type,if st.seq>0{format!("&end_seqno={}",st.seq)}else{String::new()});
                    let response = ctx.request(request, &url)?;
                    let d = data(&response)?;
                    let rows = array(d, "messages")?;
                    let mut items = vec![];
                    for row in rows {
                        let mut i = private_item(row, &st.session)?;
                        if i.sender_uid == uid.to_string() {
                            i.sender_name = "我".into()
                        } else if i.sender_uid == st.session.talker_id
                            && !st.session.name.is_empty()
                        {
                            i.sender_name = st.session.name.clone()
                        };
                        if options.contains(i.timestamp) {
                            items.push(i)
                        }
                    }
                    merge_page(store, uid, vec![st.session.clone()], items)?;
                    if !(d["has_more"].is_boolean() || d["has_more"].is_u64()) {
                        checkpoint_store(store, task, "all")?;
                        ctx.paging_failed(
                            "缺少有效 has_more 分页信息".into(),
                            json!({"reason":"invalid_paging"}),
                        )?;
                        break;
                    }
                    let next = rows
                        .iter()
                        .map(|r| num(&r["msg_seqno"]))
                        .filter(|n| *n > 0)
                        .min()
                        .unwrap_or(0);
                    let older = !rows.is_empty()
                        && options.from > 0
                        && rows.iter().all(|r| {
                            num(&r["timestamp"]) > 0 && num(&r["timestamp"]) < options.from
                        });
                    let end = !yes(&d["has_more"]) || older;
                    if !end
                        && (rows.is_empty()
                            || next == 0
                            || (st.seq > 0 && next >= st.seq)
                            || seen.contains(&next))
                    {
                        save(&store.lock().unwrap())?;
                        ctx.paging_failed(
                            "消息序列号未前进，已保留断点".into(),
                            json!({"reason":"non_advancing_cursor","sequence":st.seq}),
                        )?;
                        break;
                    }
                    seen.insert(next);
                    st.seq = next;
                    st.done = end;
                    task.sessions.insert(key.clone(), st.clone());
                    checkpoint_store(store, task, &key)?;
                    ctx.paging_accepted()?;
                    ctx.recovered()?;
                }
                Ok(())
            })();
            if let Err(e) = result {
                if e.is::<PersistFailure>() || e.to_string().contains("-101") {
                    return Err(e);
                }
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                if ctx.live {
                    return Err(e);
                }
                ctx.issue(format!("{}：{}", key, e))?;
                errors.push(format!("{}：{}", key, e));
            }
        }
    }
    Ok(errors)
}

/// File counts use attachment IDs rather than message references.
pub(crate) fn session_summaries(doc: &Doc) -> Vec<Value> {
    let mut last: BTreeMap<&str, &Item> = BTreeMap::new();
    for item in doc.items.iter().filter(|i| i.ty == "private") {
        if last
            .get(item.session.as_str())
            .is_none_or(|old| item.timestamp >= old.timestamp)
        {
            last.insert(&item.session, item);
        }
    }
    doc.sessions
        .iter()
        .map(|session| {
            let mut value = json!(session);
            let raw = &session.raw;
            value["avatar_url"] = json!([
                &raw["account_info"]["pic"],
                &raw["account_info"]["face"],
                &raw["face"],
                &raw["avatar"]
            ]
            .into_iter()
            .filter_map(Value::as_str)
            .find(|s| !s.is_empty())
            .unwrap_or(""));
            if let Some(item) = last.get(session.key.as_str()) {
                value["last_message"] = json!(item.text);
                value["last_time"] = json!(item.timestamp);
            }
            value
        })
        .collect()
}
fn attachment_exists(root: &Path, relative: &str) -> bool {
    safe_relative(relative)
        && std::fs::metadata(root.join(relative)).is_ok_and(|m| m.is_file() && m.len() > 0)
}
pub fn attachment_counts(doc: &Doc) -> (usize, usize) {
    let mut files = BTreeMap::new();
    for a in doc.items.iter().flat_map(|i| &i.attachments) {
        *files.entry(a.id.clone()).or_insert(false) |= !a.path.is_empty();
    }
    let saved = files.values().filter(|saved| **saved).count();
    (saved, files.len() - saved)
}
#[derive(Default, Debug)]
pub struct AttachmentReport {
    pub total: usize,
    pub processed: usize,
    pub saved: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}
pub fn process_configured_attachments(
    store: &Arc<Mutex<Store>>,
    uid: u64,
    cancel: &AtomicBool,
    enabled: impl Fn(&Item) -> bool,
    mut download: impl FnMut(&str, &Path) -> Result<()>,
    mut progress: impl FnMut(&AttachmentReport, &str) -> Result<()>,
) -> Result<AttachmentReport> {
    let (mut selected, root) = {
        let state = store.lock().unwrap();
        if state.doc.account_uid != uid.to_string() {
            bail!("账号已变更");
        }
        (state.doc.clone(), crate::archive::storage_root(&state.root))
    };
    selected.items.retain(|i| enabled(i));
    let mut archive =
        crate::archive::from_messages(&selected, &root, crate::archive::ResourcePolicy::load());
    let mut report = AttachmentReport::default();
    let failures = std::cell::RefCell::new(Vec::<String>::new());
    crate::archive::process_resources_observed(
        &mut archive,
        &root,
        cancel,
        |url, path| {
            if store.lock().unwrap().doc.account_uid != uid.to_string() {
                bail!("账号已变更");
            }
            let result = download(url, path);
            if let Err(error) = &result {
                if !cancel.load(Ordering::SeqCst) {
                    failures
                        .borrow_mut()
                        .push(format!("附件 {}：{error:#}", digest(url)));
                }
            }
            result
        },
        |n, total, saved, failed, key| {
            report.total = total;
            report.processed = n;
            report.saved = saved;
            report.failed = failed;
            report.errors = failures.borrow().clone();
            progress(&report, key)
        },
    )
    .context(PersistFailure)?;
    report.errors = archive
        .resources
        .values()
        .filter(|r| r["status"] == "failed")
        .map(|r| r["error"].as_str().unwrap_or_default().to_owned())
        .collect();
    let completed = crate::archive::messages(&archive)?;
    let mut state = store.lock().unwrap();
    if state.doc.account_uid != uid.to_string() {
        bail!("账号已变更，原文件已保留");
    }
    state.doc.merge(completed)?;
    save(&state).context(PersistFailure)?;
    Ok(report)
}

pub fn process_attachments(
    store: &Arc<Mutex<Store>>,
    uid: u64,
    cancel: &AtomicBool,
    enabled: impl Fn(&Item) -> bool,
    mut download: impl FnMut(&str, &Path) -> Result<()>,
    mut progress: impl FnMut(&AttachmentReport, &str),
) -> Result<AttachmentReport> {
    let pending: BTreeMap<_, _> = {
        let state = store.lock().unwrap();
        if state.doc.account_uid != uid.to_string() {
            bail!("账号已变更，附件进度已保留");
        }
        state
            .doc
            .items
            .iter()
            .filter(|i| enabled(i))
            .flat_map(|i| &i.attachments)
            .filter(|a| {
                a.path.is_empty()
                    || !attachment_exists(&crate::archive::storage_root(&state.root), &a.path)
            })
            .map(|a| (a.id.clone(), a.url.clone()))
            .collect()
    };
    let mut report = AttachmentReport {
        total: pending.len(),
        ..Default::default()
    };
    for (key, url) in pending {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        if key != digest(&url) {
            bail!("附件标识不匹配");
        }
        progress(&report, &key);
        let (root, cached) = {
            let state = store.lock().unwrap();
            if state.doc.account_uid != uid.to_string() {
                bail!("账号已变更，附件进度已保留");
            }
            let cached = state
                .doc
                .items
                .iter()
                .flat_map(|i| &i.attachments)
                .find(|a| {
                    a.id == key
                        && !a.path.is_empty()
                        && attachment_exists(&crate::archive::storage_root(&state.root), &a.path)
                })
                .map(|a| a.path.clone());
            (crate::archive::storage_root(&state.root), cached)
        };
        let relative =
            cached.unwrap_or_else(|| crate::archive::resource_path(&uid.to_string(), &key, &url));
        let dest = root.join(&relative);
        let existing = std::fs::metadata(&dest).is_ok_and(|m| m.is_file() && m.len() > 0);
        let result = if existing {
            Ok(())
        } else {
            download(&url, &dest)
        };
        // Cancellation never turns a partial request into a failed attachment.
        if cancel.load(Ordering::SeqCst) && result.is_err() {
            break;
        }
        let error = result
            .as_ref()
            .err()
            .map(ToString::to_string)
            .unwrap_or_default();
        {
            let mut state = store.lock().unwrap();
            if state.doc.account_uid != uid.to_string()
                || crate::archive::storage_root(&state.root) != root
            {
                bail!("账号或目录已变更，附件进度已保留");
            }
            for a in state
                .doc
                .items
                .iter_mut()
                .flat_map(|i| &mut i.attachments)
                .filter(|a| a.id == key)
            {
                a.path = if result.is_ok() {
                    relative.clone()
                } else {
                    String::new()
                };
                a.error = error.clone();
                if result.is_ok() {
                    a.mime = media_type(&dest).into();
                }
            }
            let groups: BTreeSet<_> = state
                .doc
                .items
                .iter()
                .filter(|i| i.attachments.iter().any(|a| a.id == key))
                .map(|i| {
                    if i.ty == "private" {
                        i.session.clone()
                    } else {
                        i.ty.clone()
                    }
                })
                .collect();
            // Persist each file before reporting it complete; a write error stops the task.
            for group in groups {
                save_group(&state, &group)?;
            }
        }
        report.processed += 1;
        if result.is_ok() {
            report.saved += 1;
        } else {
            report.failed += 1;
            report.errors.push(format!("附件 {}：{}", key, error));
        }
        progress(&report, &key);
    }
    Ok(report)
}

fn attachment_ext(url: &str) -> String {
    let p = url.split(['?', '#']).next().unwrap_or("");
    let ext = p.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    if [
        "jpg", "jpeg", "png", "gif", "webp", "avif", "bmp", "mp3", "m4a", "aac", "ogg", "wav",
        "amr", "mp4", "pdf", "zip",
    ]
    .contains(&ext.as_str())
    {
        ext
    } else {
        "bin".into()
    }
}
pub fn media_type(path: &Path) -> &'static str {
    match path.extension().and_then(|s| s.to_str()).unwrap_or("") {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "mp3" => "audio/mpeg",
        "m4a" | "aac" => "audio/mp4",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        _ => sniff_media(path),
    }
}
pub fn pause(duration: std::time::Duration, cancel: &AtomicBool) -> Result<()> {
    let start = std::time::Instant::now();
    while start.elapsed() < duration {
        if cancel.load(Ordering::SeqCst) {
            bail!("已取消")
        };
        std::thread::sleep(
            (duration - start.elapsed().min(duration)).min(std::time::Duration::from_millis(100)),
        );
    }
    if cancel.load(Ordering::SeqCst) {
        bail!("已取消")
    };
    Ok(())
}
pub fn download(url: &str, dest: &Path, cancel: &AtomicBool) -> Result<()> {
    // A separate client carries no account cookies, even across redirects.
    let parsed = reqwest::Url::parse(url)?;
    if !["http", "https"].contains(&parsed.scheme()) {
        bail!("不支持的附件协议")
    }
    let client = crate::bili::http_client()?;
    let mut response = client
        .get(url)
        .header("Referer", "https://message.bilibili.com/")
        .send()?
        .error_for_status()?;
    std::fs::create_dir_all(dest.parent().ok_or_else(|| anyhow!("无效附件目录"))?)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dest.parent().unwrap())?;
    use std::io::{Read, Write};
    let mut buffer = [0u8; 65536];
    let mut bytes = 0u64;
    loop {
        if cancel.load(Ordering::SeqCst) {
            bail!("已取消附件下载")
        };
        let n = response.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        tmp.write_all(&buffer[..n])?;
        bytes += n as u64;
    }
    if bytes == 0 {
        bail!("附件为空")
    };
    tmp.as_file().sync_all()?;
    tmp.persist(dest).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ids_status_and_unknown_messages_are_lossless() {
        let s = session(&json!({"talker_id":123,"session_type":1}))
            .unwrap()
            .unwrap();
        let item=private_item(&json!({"msg_key":18446744073709551614u64,"msg_seqno":22,"msg_status":1,"msg_type":999,"content":"{\"unknown\":42}"}),&s).unwrap();
        assert_eq!(item.id, "18446744073709551614");
        assert_eq!(item.status, 1);
        assert_eq!(item.raw["msg_key"], item.id);
        assert!(
            session(&json!({"talker_id":2,"session_type":1,"system_msg_type":7}))
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn merge_accounts_and_attachment_boundary() {
        let mut d = Doc::new(12);
        let s = session(&json!({"talker_id":2,"session_type":1}))
            .unwrap()
            .unwrap();
        let mut a = Doc::new(12);
        a.sessions.push(s.clone());
        a.items.push(
            private_item(
                &json!({"msg_key":1,"msg_seqno":1,"timestamp":10,"content":"hello"}),
                &s,
            )
            .unwrap(),
        );
        d.merge(a.clone()).unwrap();
        a.items[0].status = 1;
        d.merge(a).unwrap();
        assert_eq!(d.items.len(), 1);
        assert_eq!(d.items[0].status, 1);
        assert!(d.merge(Doc::new(99)).is_err());
        for p in [
            "../config.json",
            "attachments/../config.json",
            "attachments/a/../../x",
            "attachments\\x",
            "C:/secret",
        ] {
            assert!(!safe_relative(p));
        }
        assert!(safe_relative("attachments/abc.jpg"));
    }
    #[test]
    fn save_load_split_roundtrip_and_search() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store {
            root: base(dir.path(), 12),
            doc: Doc::new(12),
            ..Default::default()
        };
        store.doc.items.push(feed_item(&json!({"id":1,"user":{"mid":2,"nickname":"Alice"},"reply_time":1700000000,"item":{"title":"hello"}}),"replies").unwrap());
        save(&store).unwrap();
        let loaded = load(dir.path(), 12).unwrap();
        assert_eq!(loaded.doc.query("all", "", "alice", false).len(), 1);
        let bundle = dir.path().join("bundle.json");
        export_bundle(&store, &bundle).unwrap();
        let exported = crate::archive::Envelope::parse(&std::fs::read(bundle).unwrap()).unwrap();
        assert_eq!(crate::archive::messages(&exported).unwrap().items.len(), 1);
        assert_eq!(store.doc.items.len(), 1);
        let options = Options::parse(vec!["replies".into()], "2026-01-01", "2026-01-01").unwrap();
        assert_eq!(options.to - options.from, 86399);
        assert!(Options::parse(vec!["replies".into()], "2026-02-01", "2026-01-01").is_err());
    }
}

#[cfg(test)]
mod crawl_tests {
    use super::*;
    fn state(dir: &Path) -> Arc<Mutex<Store>> {
        Arc::new(Mutex::new(Store {
            root: base(dir, 12),
            doc: Doc::new(12),
            ..Default::default()
        }))
    }
    #[test]
    fn pause_resume_and_repeated_cursor_use_temporary_checkpoints() {
        let temp = tempfile::tempdir().unwrap();
        let store = state(temp.path());
        let path = temp.path().join("task.json");
        let cancel = AtomicBool::new(false);
        let options = Options::parse(vec!["replies".into()], "", "").unwrap();
        let errors=crawl_at(&store,12,options.clone(),&cancel,path.clone(),|url|{
            assert!(url.contains("reply_time=0"));cancel.store(true,Ordering::SeqCst);
            Ok(json!({"code":0,"data":{"items":[{"id":2,"reply_time":20,"item":{"title":"new"}}],"cursor":{"id":2,"time":20,"is_end":false}}}))
        },|_,_|panic!("no attachments"),|_|{}).unwrap();
        assert!(errors.is_empty());
        assert!(path.exists());
        cancel.store(false, Ordering::SeqCst);
        crawl_at(&store,12,options.clone(),&cancel,path.clone(),|url|{
            assert!(url.contains("id=2") && url.contains("reply_time=20"));
            Ok(json!({"code":0,"data":{"items":[{"id":1,"reply_time":10,"item":{"title":"old"}}],"cursor":{"id":1,"time":10,"is_end":true}}}))
        },|_,_|panic!("no attachments"),|_|{}).unwrap();
        assert!(!path.exists());
        assert_eq!(store.lock().unwrap().doc.items.len(), 2);
        let errors = crawl_at(
            &store,
            12,
            options,
            &cancel,
            path.clone(),
            |_| {
                Ok(json!({"code":0,"data":{"items":[],"cursor":{"id":1,"time":10,"is_end":false}}}))
            },
            |_, _| Ok(()),
            |_| {},
        )
        .unwrap();
        assert!(!errors.is_empty());
        assert!(path.exists());
    }
    #[test]
    fn private_sessions_skip_system_and_retry_failed_media() {
        let temp = tempfile::tempdir().unwrap();
        let store = state(temp.path());
        let path = temp.path().join("task.json");
        let cancel = AtomicBool::new(false);
        let options = Options::parse(vec!["private".into()], "", "").unwrap();
        let request = |url: &str| -> Result<Value> {
            if url.contains("fetch_session_msgs") {
                return Ok(
                    json!({"code":0,"data":{"has_more":0,"messages":[{"msg_key":99,"msg_seqno":7,"sender_uid":2,"timestamp":100,"msg_type":2,"content":"{\"url\":\"https://example.org/a.jpg\"}"}]}}),
                );
            }
            let rows = if url.contains("session_type=1&") {
                json!([{"talker_id":2,"session_type":1},{"talker_id":3,"session_type":1,"system_msg_type":7}])
            } else {
                json!([])
            };
            Ok(json!({"code":0,"data":{"has_more":0,"session_list":rows}}))
        };
        let errors = crawl_at(
            &store,
            12,
            options.clone(),
            &cancel,
            path.clone(),
            request,
            |_, _| bail!("offline"),
            |_| {},
        )
        .unwrap();
        assert_eq!(errors.len(), 1);
        assert!(path.exists());
        assert_eq!(store.lock().unwrap().doc.sessions.len(), 1);
        let mut downloads = 0;
        let errors = crawl_at(
            &store,
            12,
            options,
            &cancel,
            path.clone(),
            |_| panic!("completed pages should not be fetched twice"),
            |_, p| {
                downloads += 1;
                std::fs::create_dir_all(p.parent().unwrap())?;
                std::fs::write(p, b"image")?;
                Ok(())
            },
            |_| {},
        )
        .unwrap();
        assert!(errors.is_empty());
        assert_eq!(downloads, 1);
        assert!(!path.exists());
        assert!(
            attachment_file(&store.lock().unwrap(), &digest("https://example.org/a.jpg")).is_some()
        );
    }
}

fn sniff_media(path: &Path) -> &'static str {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    let Ok(mut file) = std::fs::File::open(path) else {
        return "application/octet-stream";
    };
    let Ok(n) = file.read(&mut bytes) else {
        return "application/octet-stream";
    };
    let b = &bytes[..n];
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if b.starts_with(&[255, 216, 255]) {
        "image/jpeg"
    } else if b.starts_with(b"GIF8") {
        "image/gif"
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        "image/webp"
    } else if b.starts_with(b"OggS") {
        "audio/ogg"
    } else if b.starts_with(b"ID3") {
        "audio/mpeg"
    } else {
        "application/octet-stream"
    }
}

#[cfg(test)]
mod group_scope_tests {
    use super::*;
    fn scopes(types: &[&str]) -> Options {
        Options::parse(types.iter().map(|s| s.to_string()).collect(), "", "").unwrap()
    }
    fn person() -> Session {
        session(&json!({"talker_id":2,"session_type":1,"name":"person"}))
            .unwrap()
            .unwrap()
    }
    fn group() -> Session {
        session(&json!({"talker_id":3,"session_type":2,"group_name":"fan group"}))
            .unwrap()
            .unwrap()
    }
    fn message(s: &Session) -> Item {
        private_item(&json!({"msg_key":9,"msg_seqno":1,"timestamp":100,"sender_uid":2,"msg_type":2,"content":"{\"url\":\"https://example.org/group.png\"}"}),s).unwrap()
    }
    #[test]
    fn independent_categories_counts_queries_and_roundtrip() {
        assert_eq!(scopes(&["private"]).session_categories(), vec![1, 2, 5, 6]);
        assert_eq!(scopes(&["groups"]).session_categories(), vec![3]);
        assert_eq!(
            scopes(&["private", "groups"]).session_categories(),
            vec![1, 2, 5, 6, 3]
        );
        let mut doc = Doc::new(12);
        doc.sessions = vec![person(), group()];
        doc.items = doc.sessions.iter().map(message).collect();
        assert_eq!(doc.summary()["counts"]["private"], 1);
        assert_eq!(doc.summary()["counts"]["groups"], 1);
        assert_eq!(doc.query("private", "", "", false).len(), 1);
        assert_eq!(doc.query("groups", "", "", false).len(), 1);
        assert_eq!(doc.query("all", "", "", false).len(), 2);
        assert!(!scopes(&["private"]).item_enabled(&doc.items[1]));
        assert!(scopes(&["groups"]).item_enabled(&doc.items[1]));
        let temp = tempfile::tempdir().unwrap();
        let state = Store {
            doc,
            root: base(temp.path(), 12),
            notice: String::new(),
        };
        save(&state).unwrap();
        assert!(state.root.join("private/user_2.json").exists());
        assert!(state.root.join("private/group_3.json").exists());
        assert!(!state.root.join("groups.json").exists());
        let loaded = load(temp.path(), 12).unwrap();
        assert_eq!(loaded.doc.query("groups", "", "fan", false).len(), 0);
        assert_eq!(loaded.doc.query("groups", "", "", false)[0].ty, "private");
        assert_eq!(
            item_summary(&loaded.doc.query("groups", "", "", false)[0])["category"],
            "groups"
        );
    }
    #[test]
    fn old_checkpoint_cannot_resume_or_download_unselected_groups() {
        let temp = tempfile::tempdir().unwrap();
        let root = base(temp.path(), 12);
        let s = group();
        let mut doc = Doc::new(12);
        doc.sessions.push(s.clone());
        doc.items.push(message(&s));
        let state = Arc::new(Mutex::new(Store {
            doc,
            root: root.clone(),
            notice: String::new(),
        }));
        save(&state.lock().unwrap()).unwrap();
        let options = scopes(&["private"]);
        let path = temp.path().join("task.json");
        let mut task = Task::open(
            12,
            options.clone(),
            path.clone(),
            root.to_string_lossy().into_owned(),
        )
        .unwrap();
        for category in options.session_categories() {
            task.lists.insert(
                category,
                Cursor {
                    done: true,
                    ..Default::default()
                },
            );
        }
        task.sessions.insert(
            s.key.clone(),
            SessionTask {
                session: s,
                seq: 0,
                done: false,
            },
        );
        task.save().unwrap();
        let errors = crawl_at(
            &state,
            12,
            options,
            &AtomicBool::new(false),
            path.clone(),
            |_| panic!("unselected group must not be requested"),
            |_, _| panic!("unselected group attachments must not be downloaded"),
            |_| {},
        )
        .unwrap();
        assert!(errors.is_empty());
        assert!(!path.exists());
        assert_eq!(state.lock().unwrap().doc.items.len(), 1);
        assert!(root.join("private/group_3.json").exists());
    }
    #[test]
    fn groups_only_collects_and_downloads_group_messages() {
        let temp = tempfile::tempdir().unwrap();
        let state = Arc::new(Mutex::new(Store {
            doc: Doc::new(12),
            root: base(temp.path(), 12),
            notice: String::new(),
        }));
        let mut downloads = 0;
        let errors=crawl_at(&state,12,scopes(&["groups"]),&AtomicBool::new(false),temp.path().join("task.json"),|url|{
            if url.contains("get_sessions") {
                assert!(url.contains("session_type=3&"));
                Ok(json!({"code":0,"data":{"has_more":0,"session_list":[{"talker_id":3,"session_type":2,"group_name":"fans"},{"talker_id":2,"session_type":1,"name":"unexpected person"}]}}))
            }else{
                assert!(url.contains("fetch_session_msgs") && url.contains("session_type=2&"));
                Ok(json!({"code":0,"data":{"has_more":0,"messages":[{"msg_key":9,"msg_seqno":1,"timestamp":100,"msg_type":2,"content":"{\"url\":\"https://example.org/group.png\"}"}]}}))
            }
        },|_,path|{downloads+=1;std::fs::create_dir_all(path.parent().unwrap())?;std::fs::write(path,b"image")?;Ok(())},|_|{}).unwrap();
        assert!(errors.is_empty());
        assert_eq!(downloads, 1);
        let state = state.lock().unwrap();
        assert_eq!(state.doc.sessions.len(), 1);
        assert_eq!(state.doc.summary()["counts"]["private"], 0);
        assert_eq!(state.doc.summary()["counts"]["groups"], 1);
    }
    #[test]
    fn selecting_both_deduplicates_sessions_across_list_categories() {
        let temp = tempfile::tempdir().unwrap();
        let state = Arc::new(Mutex::new(Store {
            doc: Doc::new(12),
            root: base(temp.path(), 12),
            notice: String::new(),
        }));
        let mut histories = 0;
        let errors=crawl_at(&state,12,scopes(&["private","groups"]),&AtomicBool::new(false),temp.path().join("task.json"),|url|{
            if url.contains("get_sessions") {
                let rows=if url.contains("session_type=1&") {json!([{"talker_id":2,"session_type":1,"name":"person"},{"talker_id":3,"session_type":2,"group_name":"fans"}])}else if url.contains("session_type=3&"){json!([{"talker_id":3,"session_type":2,"group_name":"fans"}])}else{json!([])};
                Ok(json!({"code":0,"data":{"has_more":0,"session_list":rows}}))
            }else{assert!(url.contains("fetch_session_msgs"));histories+=1;Ok(json!({"code":0,"data":{"has_more":0,"messages":[{"msg_key":9,"msg_seqno":1,"timestamp":100,"content":"text"}]}}))}
        },|_,_|panic!("no attachments"),|_|{}).unwrap();
        assert!(errors.is_empty());
        assert_eq!(histories, 2);
        assert_eq!(state.lock().unwrap().doc.items.len(), 2);
    }
}

/// Move old message checkpoints once at application startup; never overwrite a different file.
#[cfg(test)]
pub fn migrate_checkpoint_directory() -> Result<()> {
    migrate_checkpoint_directory_in(&crate::checkpoint::data_dir()?)
}
#[cfg(test)]
fn migrate_checkpoint_directory_in(data: &Path) -> Result<()> {
    let old = data.join("message-checkpoints");
    let new = data.join("checkpoints/messages");
    if !old.exists() {
        return Ok(());
    }
    let mut conflicts = Vec::new();
    for entry in std::fs::read_dir(&old)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json")
            || !path
                .file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(valid_id)
        {
            continue;
        }
        std::fs::create_dir_all(&new)?;
        let dest = new.join(entry.file_name());
        if dest.exists() {
            if std::fs::read(&path)? == std::fs::read(&dest)? {
                std::fs::remove_file(&path)?;
            } else {
                conflicts.push(entry.file_name().to_string_lossy().into_owned());
            }
        } else {
            std::fs::rename(&path, &dest)?;
        }
    }
    if std::fs::read_dir(&old)?.next().is_none() {
        std::fs::remove_dir(&old)?;
    }
    if !conflicts.is_empty() {
        bail!(
            "新旧消息断点不同，已保留两个文件，未覆盖：{}",
            conflicts.join("、")
        )
    }
    Ok(())
}

#[cfg(test)]
mod checkpoint_directory_tests {
    use super::*;
    #[test]
    fn moves_once_and_preserves_conflicts_and_other_files() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("message-checkpoints");
        let new = temp.path().join("checkpoints/messages");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("12.json"), b"checkpoint").unwrap();
        migrate_checkpoint_directory_in(temp.path()).unwrap();
        assert_eq!(std::fs::read(new.join("12.json")).unwrap(), b"checkpoint");
        assert!(!old.exists());
        migrate_checkpoint_directory_in(temp.path()).unwrap();
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("12.json"), b"different").unwrap();
        std::fs::write(old.join("notes.txt"), b"keep").unwrap();
        assert!(migrate_checkpoint_directory_in(temp.path()).is_err());
        assert_eq!(std::fs::read(old.join("12.json")).unwrap(), b"different");
        assert_eq!(std::fs::read(new.join("12.json")).unwrap(), b"checkpoint");
        assert!(old.join("notes.txt").exists());
    }
}

#[cfg(test)]
mod attachment_processing_tests {
    use super::*;

    #[test]
    fn attachment_inventory_groups_and_rechecks_local_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("attachments")).unwrap();
        let file = root.join("attachments/ok.png");
        std::fs::write(&file, b"image").unwrap();
        let ok = Attachment {
            id: "ok".into(), url: "https://example.test/ok.png".into(),
            path: "attachments/ok.png".into(), ..Default::default()
        };
        let missing = Attachment {
            id: "missing".into(), url: "https://example.test/missing.mp3".into(),
            path: "attachments/missing.mp3".into(), ..Default::default()
        };
        let cover = Attachment {
            id: "cover".into(), url: "https://example.test/cover.jpg".into(), ..Default::default()
        };
        let mut doc = Doc::new(12);
        doc.items = vec![
            Item { ty: "private".into(), session: "user_2".into(), attachments: vec![ok.clone()], ..Default::default() },
            Item { ty: "private".into(), session: "group_3".into(), attachments: vec![ok.clone(), missing], ..Default::default() },
            Item { ty: "replies".into(), raw: json!({"item":{"image":cover.url}}), attachments: vec![ok, cover], ..Default::default() },
        ];
        let mut inventory = AttachmentInventory::default();
        inventory.add(&doc);
        let counts = inventory.counts(root, "12");
        assert_eq!(counts["private"], json!({"saved":1,"pending":1}));
        assert_eq!(counts["replies"], json!({"saved":1,"pending":0}));
        assert_eq!(counts["likes"], json!({"saved":0,"pending":0}));
        std::fs::remove_file(file).unwrap();
        assert_eq!(inventory.counts(root, "12")["private"], json!({"saved":0,"pending":2}));
    }

    fn state(root: &Path) -> Arc<Mutex<Store>> {
        let a = Attachment {
            id: digest("https://example.test/a.jpg"),
            url: "https://example.test/a.jpg".into(),
            ..Default::default()
        };
        let b = Attachment {
            id: digest("https://example.test/b.jpg"),
            url: "https://example.test/b.jpg".into(),
            ..Default::default()
        };
        let mut doc = Doc::new(12);
        doc.items = vec![
            Item {
                id: "1".into(),
                ty: "replies".into(),
                attachments: vec![a.clone(), b],
                ..Default::default()
            },
            Item {
                id: "2".into(),
                ty: "likes".into(),
                attachments: vec![a],
                ..Default::default()
            },
        ];
        Arc::new(Mutex::new(Store {
            doc,
            root: base(root, 12),
            ..Default::default()
        }))
    }
    fn file(path: &Path) -> Result<()> {
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, b"image")?;
        Ok(())
    }
    #[test]
    fn deduplicated_files_retry_and_reload_update_every_reference() {
        let dir = tempfile::tempdir().unwrap();
        let store = state(dir.path());
        let cancel = AtomicBool::new(false);
        let mut calls = 0;
        assert_eq!(attachment_counts(&store.lock().unwrap().doc), (0, 2));
        let first = process_attachments(
            &store,
            12,
            &cancel,
            |_| true,
            |url, path| {
                calls += 1;
                if url.ends_with("b.jpg") {
                    bail!("temporary failure");
                }
                file(path)
            },
            |_, _| {},
        )
        .unwrap();
        assert_eq!(
            (first.total, first.saved, first.failed, calls),
            (2, 1, 1, 2)
        );
        assert_eq!(attachment_counts(&store.lock().unwrap().doc), (1, 1));
        assert!(store
            .lock()
            .unwrap()
            .doc
            .items
            .iter()
            .flat_map(|i| &i.attachments)
            .filter(|a| a.url.ends_with("a.jpg"))
            .all(|a| !a.path.is_empty()));
        let next = process_attachments(
            &store,
            12,
            &cancel,
            |_| true,
            |_, path| file(path),
            |_, _| {},
        )
        .unwrap();
        assert_eq!((next.total, next.saved), (1, 1));
        assert_eq!(
            attachment_counts(&load(dir.path(), 12).unwrap().doc),
            (2, 0)
        );
    }
    #[test]
    fn cancellation_keeps_saved_results_and_cached_files_resume_without_network() {
        let dir = tempfile::tempdir().unwrap();
        let store = state(dir.path());
        let cancel = AtomicBool::new(false);
        let first = process_attachments(
            &store,
            12,
            &cancel,
            |_| true,
            |_, path| {
                file(path)?;
                cancel.store(true, Ordering::SeqCst);
                Ok(())
            },
            |_, _| {},
        )
        .unwrap();
        assert_eq!(first.saved, 1);
        cancel.store(false, Ordering::SeqCst);
        let (key, root) = {
            let state = store.lock().unwrap();
            (
                state
                    .doc
                    .items
                    .iter()
                    .flat_map(|i| &i.attachments)
                    .find(|a| a.path.is_empty())
                    .unwrap()
                    .id
                    .clone(),
                crate::archive::storage_root(&state.root),
            )
        };
        file(&root.join(format!("resources/12/{key}.jpg"))).unwrap();
        let resumed = process_attachments(
            &store,
            12,
            &cancel,
            |_| true,
            |_, _| panic!("reuse existing download"),
            |_, _| {},
        )
        .unwrap();
        assert_eq!(resumed.saved, 1);
        assert_eq!(attachment_counts(&store.lock().unwrap().doc), (2, 0));
        let loaded = load(dir.path(), 12).unwrap();
        let missing = loaded.doc.items[0].attachments[0].path.clone();
        std::fs::remove_file(root.join(missing)).unwrap();
        assert_eq!(
            attachment_counts(&load(dir.path(), 12).unwrap().doc),
            (1, 1)
        );
    }
    #[test]
    fn account_changes_and_persistence_failures_stop_processing() {
        let dir = tempfile::tempdir().unwrap();
        let store = state(dir.path());
        assert!(process_attachments(
            &store,
            99,
            &AtomicBool::new(false),
            |_| true,
            |_, _| panic!("wrong account"),
            |_, _| {}
        )
        .is_err());
        let changing = store.clone();
        assert!(process_attachments(
            &store,
            12,
            &AtomicBool::new(false),
            |_| true,
            |_, path| {
                file(path)?;
                changing.lock().unwrap().doc = Doc::new(99);
                Ok(())
            },
            |_, _| {}
        )
        .is_err());
        assert_eq!(store.lock().unwrap().doc.account_uid, "99");
        let store = state(dir.path());
        let root = store.lock().unwrap().root.clone();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("replies.json"), b"invalid archive").unwrap();
        assert!(process_attachments(
            &store,
            12,
            &AtomicBool::new(false),
            |_| true,
            |_, path| file(path),
            |_, _| {}
        )
        .is_err());
    }
    #[test]
    fn session_summary_is_read_only_and_uses_latest_message() {
        let mut doc = Doc::new(12);
        doc.sessions.push(Session {
            key: "user_2".into(),
            talker_id: "2".into(),
            session_type: 1,
            name: "person".into(),
            raw: json!({"account_info":{"pic":"https://example.test/avatar.jpg"}}),
            ..Default::default()
        });
        for (id, time, text) in [("1", 1, "old"), ("2", 2, "latest")] {
            doc.items.push(Item {
                id: id.into(),
                ty: "private".into(),
                session: "user_2".into(),
                timestamp: time,
                text: text.into(),
                ..Default::default()
            });
        }
        let before = serde_json::to_string(&doc).unwrap();
        let summary = doc.summary();
        assert_eq!(summary["sessions"][0]["last_message"], "latest");
        assert_eq!(summary["sessions"][0]["last_time"], 2);
        assert_eq!(
            summary["sessions"][0]["avatar_url"],
            "https://example.test/avatar.jpg"
        );
        assert_eq!(before, serde_json::to_string(&doc).unwrap());
    }
    #[test]
    fn like_count_order_is_filtered_stable_and_precedes_paging() {
        let mut doc = Doc::new(12);
        for (id, count, time, text) in [
            ("1", 3, 10, "keep"),
            ("2", 9, 0, "keep"),
            ("3", 9, 20, "keep"),
            ("4", 9, 20, "keep"),
            ("5", 0, 100, "keep"),
            ("6", u64::MAX, 30, "excluded"),
        ] {
            doc.items.push(Item {
                id: id.into(),
                ty: "likes".into(),
                like_count: count,
                timestamp: time,
                text: text.into(),
                ..Default::default()
            });
        }
        let ids = |order| {
            doc.query_ordered("likes", "", "keep", order)
                .into_iter()
                .map(|i| i.id.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids("likes_desc"), ["3", "4", "2", "1", "5"]);
        assert_eq!(ids("likes_asc"), ["5", "1", "3", "4", "2"]);
        assert_eq!(ids("new"), ["5", "3", "4", "1", "2"]);
        assert_eq!(ids("old"), ["1", "3", "4", "5", "2"]);
        let all = doc.query_ordered("likes", "", "", "likes_desc");
        assert_eq!(all[0].id, "6");
        let pages: Vec<_> = all
            .chunks(2)
            .flat_map(|p| p.iter().map(|i| i.id.as_str()))
            .collect();
        assert_eq!(pages, ["6", "3", "4", "2", "1", "5"]);
        doc.items.push(Item {
            id: "7".into(),
            ty: "replies".into(),
            timestamp: 7,
            ..Default::default()
        });
        doc.items.push(Item {
            id: "8".into(),
            ty: "replies".into(),
            timestamp: 8,
            ..Default::default()
        });
        assert_eq!(
            doc.query_ordered("replies", "", "", "likes_asc")
                .iter()
                .map(|i| i.id.as_str())
                .collect::<Vec<_>>(),
            ["8", "7"]
        );
    }
    #[test]
    fn statistics_count_notices_and_separate_private_groups() {
        let mut doc = Doc::new(12);
        assert!(doc.counts().values().all(|n| *n == 0));
        for (ty, session) in [
            ("private", "user_2"),
            ("private", "group_3"),
            ("likes", ""),
            ("likes", ""),
            ("replies", ""),
            ("mentions", ""),
        ] {
            doc.items.push(Item {
                ty: ty.into(),
                session: session.into(),
                like_count: 99,
                ..Default::default()
            });
        }
        let counts = doc.counts();
        assert_eq!(
            (
                counts["private"],
                counts["groups"],
                counts["likes"],
                counts["replies"],
                counts["mentions"]
            ),
            (1, 1, 2, 1, 1)
        );
    }
}
