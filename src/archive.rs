//! Versioned archival boundary. Runtime view models are deliberately separate.
use anyhow::{anyhow, bail, Context, Result};
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

pub const FORMAT: &str = "bili-archive";
pub const VERSION: u32 = 1;
static ACCOUNT: AtomicU64 = AtomicU64::new(0);
pub fn set_account(uid: u64) {
    ACCOUNT.store(uid, Ordering::SeqCst);
}
pub fn account() -> u64 {
    ACCOUNT.load(Ordering::SeqCst)
}
#[derive(Clone, Default, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct ResourcePolicy {
    pub avatars: bool,
    pub covers: bool,
    pub content_images: bool,
}
impl ResourcePolicy {
    pub fn enabled(&self, ty: &str) -> bool {
        match ty {
            "private_image" | "message_attachment" => true,
            "avatar" => self.avatars,
            "cover" => self.covers,
            "content_image" => self.content_images,
            _ => false,
        }
    }
    pub fn load() -> Self {
        config()
            .get("resource_policy")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default()
    }
}
fn config() -> Value {
    crate::checkpoint::data_dir()
        .ok()
        .and_then(|p| std::fs::read(p.join("config.json")).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}
pub fn configured_root() -> PathBuf {
    if let Some(active)=crate::library::active(){return root(&active.root);}
    let value = config();
    let path = value["export_dir"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            crate::checkpoint::data_dir()
                .unwrap_or_default()
                .join("exports")
        });
    root(&path)
}
pub fn root(path: &Path) -> PathBuf {
    if path.file_name().is_some_and(|s| s == "archive-v1") {
        path.into()
    } else {
        path.join("archive-v1")
    }
}
pub fn storage_root(path: &Path) -> PathBuf {
    path.ancestors()
        .find(|p| p.file_name().is_some_and(|s| s == "archive-v1"))
        .unwrap_or(path)
        .into()
}
pub fn hash(s: &str) -> String {
    format!("{:x}", Md5::digest(s.as_bytes()))
}
fn uid(v: &Value) -> String {
    v.as_str()
        .map(str::to_owned)
        .or_else(|| v.as_u64().map(|n| n.to_string()))
        .unwrap_or_default()
}
fn url(s: &str) -> String {
    crate::message_subjects::image_url(s)
}
pub fn safe_path(s: &str) -> bool {
    !s.is_empty()
        && !s.contains('\\')
        && !Path::new(s).is_absolute()
        && Path::new(s)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}
pub fn resource_path(account: &str, id: &str, source: &str) -> String {
    let ext = source
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let ext = if [
        "jpg", "jpeg", "png", "gif", "webp", "avif", "bmp", "mp3", "m4a", "aac", "ogg", "wav",
        "amr", "mp4", "pdf", "zip",
    ]
    .contains(&ext.as_str())
    {
        ext.as_str()
    } else {
        "bin"
    };
    format!("resources/{account}/{id}.{ext}")
}
fn timestamp(n: i64) -> Value {
    if n <= 0 {
        Value::Null
    } else {
        json!(seconds(n as u64))
    }
}
pub fn seconds(n: u64) -> u64 {
    if n >= 1_000_000_000_000_000 {
        n / 1_000_000
    } else if n >= 1_000_000_000_000 {
        n / 1000
    } else {
        n
    }
}
pub fn merge_metadata(old: &mut Value, new: &Value) {
    if let Value::Object(m) = new {
        if !old.is_object() {
            *old = json!({});
        }
        for (key, value) in m {
            merge_metadata(&mut old[key], value);
        }
    } else if !new.is_null() && new.as_str() != Some("") {
        *old = new.clone();
    }
}
fn compact_collection(value: &Value, count: usize) -> Value {
    let legacy = value.get("errors").is_some() || value.get("scope").is_some();
    json!({"status":if !legacy && ["partial","exhausted","cancelled","failed"].contains(&value["status"].as_str().unwrap_or_default()){value["status"].clone()}else{json!("partial")},"archived_total":count,"reported_total":if legacy{Value::Null}else{value["reported_total"].clone()},"exhausted":if legacy{Value::Null}else{value["exhausted"].clone()}})
}
fn message_scope(source: &Value) -> String {
    if let Some(key) = source["session_key"].as_str().filter(|s| !s.is_empty()) {
        return key.into();
    }
    if let Some(key) = source["category"]
        .as_str()
        .filter(|s| crate::messages::TYPES.contains(s) && *s != "private")
    {
        return key.into();
    }
    if let Some(keys) = source["categories"].as_array().filter(|v| v.len() == 1) {
        return keys[0].as_str().unwrap_or_default().into();
    }
    String::new()
}
fn metadata(doc: &Envelope) -> Value {
    json!({"meta":doc.meta,"source":doc.source,"users":doc.users,"subjects":doc.subjects,"resources":doc.resources,"collection":compact_collection(&doc.collection,doc.items.len())})
}
pub fn string_ids(v: &mut Value) {
    match v {
        Value::Object(m) => {
            for (key, value) in m {
                if [
                    "id",
                    "uid",
                    "mid",
                    "aid",
                    "rpid",
                    "oid",
                    "parent",
                    "root",
                    "owner_mid",
                    "dynamic_id",
                    "talker_id",
                    "sender_uid",
                    "receiver_id",
                    "msg_key",
                    "msg_seqno",
                ]
                .contains(&key.as_str())
                    || key.ends_with("_id")
                    || key.ends_with("_uid")
                {
                    if let Some(n) = value.as_u64() {
                        *value = json!(n.to_string());
                    }
                }
                string_ids(value);
            }
        }
        Value::Array(a) => {
            for x in a {
                string_ids(x)
            }
        }
        _ => (),
    }
}
fn number_ids(v: &mut Value) {
    match v {
        Value::Object(m) => {
            for (key, value) in m {
                if ["aid", "rpid", "mid", "owner_mid", "dynamic_id", "parent"]
                    .contains(&key.as_str())
                {
                    if let Some(n) = value.as_str().and_then(|s| s.parse::<u64>().ok()) {
                        *value = json!(n);
                    }
                }
                if key != "raw" {
                    number_ids(value);
                }
            }
        }
        Value::Array(a) => {
            for x in a {
                number_ids(x)
            }
        }
        _ => (),
    }
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Envelope {
    pub format: String,
    pub version: u32,
    pub meta: Value,
    pub source: Value,
    pub users: BTreeMap<String, Value>,
    pub subjects: BTreeMap<String, Value>,
    pub sessions: Vec<Value>,
    pub items: Vec<Value>,
    pub resources: BTreeMap<String, Value>,
    pub collection: Value,
}
impl Envelope {
    fn new(account: u64, ty: &str) -> Self {
        Self {
            format: FORMAT.into(),
            version: VERSION,
            meta: json!({"account_uid":if account==0 {Value::Null}else{json!(account.to_string())},"app_version":env!("CARGO_PKG_VERSION"),"exported_at":chrono::Local::now().to_rfc3339()}),
            source: json!({"type":ty}),
            users: BTreeMap::new(),
            subjects: BTreeMap::new(),
            sessions: vec![],
            items: vec![],
            resources: BTreeMap::new(),
            collection: json!({"status":"partial","reported_total":null,"archived_total":0,"exhausted":null}),
        }
    }
    pub fn validate(&self) -> Result<()> {
        if self.format != FORMAT || self.version != VERSION {
            bail!("归档版本不受支持，请重新采集");
        }
        if self.source["type"] == "messages"
            && self.meta["account_uid"]
                .as_str()
                .and_then(|s| s.parse::<u64>().ok())
                .is_none_or(|n| n == 0)
        {
            bail!("消息归档账号无效");
        }
        if !["comments", "messages"].contains(&self.source["type"].as_str().unwrap_or_default()) {
            bail!("归档来源类型无效");
        }
        for (key, subject) in &self.subjects {
            if subject["key"].as_str() != Some(key.as_str())
                || subject["id"].as_str().is_none()
                || !["video", "dynamic", "article", "audio"]
                    .contains(&subject["type"].as_str().unwrap_or_default())
            {
                bail!("主题标识或类型无效");
            }
        }
        for item in &self.items {
            for name in ["sender_uid", "user_uid", "parent_id", "root_id"] {
                if !item[name].is_null() && !item[name].is_string() {
                    bail!("归档标识必须使用字符串");
                }
            }
        }
        let comment_roots: BTreeSet<(String, String)> = self
            .items
            .iter()
            .filter(|i| i["type"] == "comment")
            .map(|i| {
                (
                    i["subject_key"].as_str().unwrap_or_default().into(),
                    i["id"].as_str().unwrap_or_default().into(),
                )
            })
            .collect();
        let mut keys = BTreeSet::new();
        for item in &self.items {
            let is_comment = matches!(item["type"].as_str(), Some("comment" | "comment_reply"));
            if (self.source["type"] == "comments") != is_comment {
                bail!("归档项目与来源类型不匹配");
            }
            let id = item["id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow!("归档项目缺少字符串 ID"))?;
            let key = format!(
                "{}:{}:{}:{}",
                item["type"], item["subject_key"], item["session_key"], id
            );
            if !keys.insert(key) {
                bail!("归档存在重复项目");
            }
            if let Some(key) = item["subject_key"].as_str().filter(|s| !s.is_empty()) {
                if !self.subjects.contains_key(key) {
                    bail!("归档主题引用缺失");
                }
            }
            if let Some(key) = item["session_key"].as_str().filter(|s| !s.is_empty()) {
                if !self.sessions.iter().any(|s| s["key"].as_str() == Some(key)) {
                    bail!("归档会话引用缺失");
                }
            }
        }
        for item in &self.items {
            if ![
                "comment",
                "comment_reply",
                "private",
                "replies",
                "mentions",
                "likes",
            ]
            .contains(&item["type"].as_str().unwrap_or_default())
            {
                bail!("未知归档项目类型");
            }
            if !item["timestamp"].is_null()
                && item["timestamp"].as_i64().is_none()
                && item["timestamp"].as_u64().is_none()
            {
                bail!("归档时间必须为秒级整数");
            }
            for id in item["resource_ids"].as_array().into_iter().flatten() {
                if !id
                    .as_str()
                    .is_some_and(|id| self.resources.contains_key(id))
                {
                    bail!("归档资源引用缺失");
                }
            }
            if item["type"] == "comment_reply"
                && !comment_roots.contains(&(
                    item["subject_key"].as_str().unwrap_or_default().into(),
                    item["root_id"].as_str().unwrap_or_default().into(),
                ))
            {
                bail!("归档根评论引用缺失");
            }
        }
        for (key, res) in &self.resources {
            if !["not_requested", "pending", "saved", "failed"]
                .contains(&res["status"].as_str().unwrap_or_default())
            {
                bail!("资源状态无效");
            }
            if res["type"] == "private_image" && res["required"] != true {
                bail!("私信图片必须离线保存");
            }
            if res["status"] == "saved"
                && (res["path"].as_str().unwrap_or_default().is_empty()
                    || res["size"].as_u64().unwrap_or(0) == 0
                    || res["checksum"].as_str().unwrap_or_default().len() != 32)
            {
                bail!("已保存资源缺少完整性元数据");
            }
            let source = res["url"].as_str().unwrap_or_default();
            if url(source).is_empty() || *key != hash(source) {
                bail!("归档资源标识无效");
            }
            if let Some(path) = res["path"].as_str().filter(|s| !s.is_empty()) {
                if !safe_path(path) || !path.starts_with("resources/") {
                    bail!("归档资源路径越界");
                }
                let owner = self.meta["account_uid"].as_str().unwrap_or("public");
                if !path.starts_with(&format!("resources/{owner}/"))
                    || Path::new(path).file_stem().and_then(|s| s.to_str()) != Some(key.as_str())
                {
                    bail!("归档资源账号或文件名不匹配");
                }
            }
        }
        Ok(())
    }
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        Self::from_value(serde_json::from_slice(bytes)?)
    }
    pub(crate) fn from_value(value: Value) -> Result<Self> {
        if value["format"] == "bili-collection-report" {
            bail!("这是采集报告，不是数据归档");
        }
        if value["format"] != FORMAT {
            bail!("归档格式不受支持，请重新采集");
        }
        let doc: Self = serde_json::from_value(value)?;
        doc.validate()?;
        Ok(doc)
    }
    fn user(&mut self, key: &str, name: &str, avatar: &str, builder: &mut Resources) {
        if key.is_empty() || key == "0" {
            return;
        }
        let photo = builder.add("avatar", avatar, None);
        let entry = self
            .users
            .entry(key.into())
            .or_insert_with(|| json!({"uid":key,"name":name,"avatar_resource_id":photo}));
        if key.parse::<u64>().is_err() {
            entry["uid"] = Value::Null;
        }
        if entry["name"].as_str().unwrap_or_default().is_empty() {
            entry["name"] = json!(name);
        }
        if !photo.is_empty() {
            entry["avatar_resource_id"] = json!(photo);
        }
    }
}
struct Resources {
    verify_checksums: bool,
    root: PathBuf,
    account: String,
    policy: ResourcePolicy,
    entries: BTreeMap<String, Value>,
    catalog: BTreeMap<String, Value>,
}
impl Resources {
    fn new(root: &Path, account: u64, policy: ResourcePolicy) -> Self {
        Self {
            verify_checksums: true,
            root: root.into(),
            account: if account == 0 {
                "public".into()
            } else {
                account.to_string()
            },
            policy,
            entries: BTreeMap::new(),
            catalog: std::fs::read(
                root.join("resources")
                    .join(if account == 0 {
                        "public".into()
                    } else {
                        account.to_string()
                    })
                    .join("index.json"),
            )
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default(),
        }
    }
    fn add(&mut self, ty: &str, source: &str, existing: Option<&str>) -> String {
        let source = url(source);
        if source.is_empty() {
            return String::new();
        }
        let key = hash(&source);
        let path = existing
            .filter(|p| safe_path(p) && self.root.join(p).is_file())
            .map(str::to_owned)
            .unwrap_or_else(|| resource_path(&self.account, &key, &source));
        let saved =
            std::fs::metadata(self.root.join(&path)).is_ok_and(|m| m.is_file() && m.len() > 0);
        let cached = self.catalog.get(&key).cloned().unwrap_or(Value::Null);
        let requested = self.policy.enabled(ty);
        let entry=self.entries.entry(key.clone()).or_insert_with(||json!({"id":key,"type":ty,"url":source,"path":if saved {path.clone()}else{String::new()},"status":if saved {"saved"}else if requested {"pending"}else{"not_requested"},"required":ty=="private_image","mime":"","size":0,"checksum":"","error":""}));
        if requested && entry["status"] == "not_requested" {
            entry["status"] = json!("pending");
            entry["type"] = json!(ty);
        }
        if requested && !saved && cached["status"] == "failed" {
            entry["status"] = json!("failed");
            entry["error"] = cached["error"].clone();
        }
        if saved && cached["checksum"].is_string() {
            entry["checksum"] = cached["checksum"].clone();
        }
        if ty == "private_image" {
            entry["required"] = json!(true);
            entry["type"] = json!(ty);
            if !saved && entry["status"] != "failed" {
                entry["status"] = json!("pending");
            }
        }
        if saved {
            entry["path"] = json!(path);
            entry["status"] = json!("saved");
            if let Ok(m) =
                std::fs::metadata(self.root.join(entry["path"].as_str().unwrap_or_default()))
            {
                entry["size"] = json!(m.len());
            }
            if self.verify_checksums && entry["checksum"].as_str().unwrap_or_default().is_empty() {
                match file_hash(&self.root.join(entry["path"].as_str().unwrap_or_default())) {
                    Ok(sum) => entry["checksum"] = json!(sum),
                    Err(error) => {
                        entry["status"] = json!(if requested || ty == "private_image" {
                            "failed"
                        } else {
                            "not_requested"
                        });
                        entry["path"] = json!("");
                        entry["error"] = json!(error.to_string());
                    }
                }
            }
            entry["mime"] = json!(crate::messages::media_type(
                &self.root.join(entry["path"].as_str().unwrap_or_default())
            ));
        }
        key
    }
}
fn actor(doc: &mut Envelope, b: &mut Resources, user: &Value, fallback: &str) {
    let key = [&user["mid"], &user["uid"]]
        .into_iter()
        .map(uid)
        .find(|s| !s.is_empty())
        .unwrap_or_else(|| fallback.into());
    let name = [&user["nickname"], &user["uname"], &user["name"]]
        .into_iter()
        .filter_map(Value::as_str)
        .find(|s| !s.is_empty())
        .unwrap_or_default();
    let photo = [&user["avatar"], &user["face"]]
        .into_iter()
        .filter_map(Value::as_str)
        .find(|s| !s.is_empty())
        .unwrap_or_default();
    doc.user(&key, name, photo, b);
}
pub fn from_comments(
    input: &crate::model::ArchiveDoc,
    account: u64,
    root: &Path,
    policy: ResourcePolicy,
) -> Envelope {
    let mut doc = Envelope::new(account, "comments");
    let mut b = Resources::new(root, account, policy);
    if let Ok(saved) =
        serde_json::from_value::<BTreeMap<String, Value>>(input.archive["resources"].clone())
    {
        for (key, value) in saved {
            b.catalog.entry(key).or_insert(value);
        }
    }
    doc.collection = compact_collection(&input.archive["collection"], 0);
    for video in &input.videos {
        let key = format!("{}:{}", crate::export::category(video), video.aid);
        let mut subject = serde_json::to_value(video).unwrap_or_default();
        subject.as_object_mut().unwrap().remove("comments");
        let cover = b.add("cover", &video.cover, None);
        let images: Vec<_> = video
            .text_pics
            .iter()
            .map(|u| b.add("content_image", u, None))
            .filter(|s| !s.is_empty())
            .collect();
        subject["type"] = json!(match video.reply_type() {
            11 | 17 => "dynamic",
            12 => "article",
            14 => "audio",
            _ => "video",
        });
        subject["key"] = json!(key);
        subject["id"] = json!(video.aid.to_string());
        subject["cover_resource_id"] = json!(cover);
        subject["image_resource_ids"] = json!(images);
        doc.subjects.insert(key.clone(), subject);
        for comment in &video.comments {
            doc.user(
                &comment.mid.to_string(),
                &comment.uname,
                &comment.avatar,
                &mut b,
            );
            let images: Vec<_> = comment
                .pictures
                .iter()
                .map(|u| b.add("content_image", u, None))
                .filter(|s| !s.is_empty())
                .collect();
            let mut fields = serde_json::to_value(comment).unwrap_or_default();
            fields.as_object_mut().unwrap().remove("replies");
            let raw = fields
                .as_object_mut()
                .unwrap()
                .remove("raw")
                .unwrap_or(Value::Null);
            doc.items.push(json!({"id":comment.rpid.to_string(),"type":"comment","subject_key":key,"user_uid":comment.mid.to_string(),"parent_id":null,"root_id":comment.rpid.to_string(),"text":comment.content,"timestamp":timestamp(comment.ctime),"resource_ids":images,"fields":fields,"raw":raw}));
            for reply in &comment.replies {
                doc.user(&reply.mid.to_string(), &reply.uname, &reply.avatar, &mut b);
                let images: Vec<_> = reply
                    .pictures
                    .iter()
                    .map(|u| b.add("content_image", u, None))
                    .filter(|s| !s.is_empty())
                    .collect();
                let mut fields = serde_json::to_value(reply).unwrap_or_default();
                let raw = fields
                    .as_object_mut()
                    .unwrap()
                    .remove("raw")
                    .unwrap_or(Value::Null);
                doc.items.push(json!({"id":reply.rpid.to_string(),"type":"comment_reply","subject_key":key,"user_uid":reply.mid.to_string(),"root_id":comment.rpid.to_string(),"parent_id":if reply.parent==0 {comment.rpid.to_string()}else{reply.parent.to_string()},"text":reply.content,"timestamp":timestamp(reply.ctime),"resource_ids":images,"fields":fields,"raw":raw}));
            }
        }
    }
    doc.source["subject_keys"] = json!(doc.subjects.keys().collect::<Vec<_>>());
    if let Some(video) = input.videos.first().filter(|_| input.videos.len() == 1) {
        doc.source["owner_uid"] = json!(video.owner_mid.to_string());
        doc.source["category"] = json!(crate::export::category(video));
    }
    doc.collection["archived_total"] = json!(doc.items.len());
    doc.collection["reported_total"] =
        json!(input.videos.iter().map(|v| v.comment_total).sum::<u64>());
    doc.resources = b.entries;
    let mut value = serde_json::to_value(&doc).unwrap();
    string_ids(&mut value);
    serde_json::from_value(value).unwrap()
}
pub fn attachment_type(
    item: &crate::messages::Item,
    a: &crate::messages::Attachment,
) -> &'static str {
    let path = a
        .url
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let image = a.mime.starts_with("image/")
        || [".jpg", ".jpeg", ".png", ".gif", ".webp", ".avif", ".bmp"]
            .iter()
            .any(|e| path.ends_with(e));
    let content = if let Some(s) = item.raw["content"].as_str() {
        serde_json::from_str::<Value>(s).unwrap_or_default()
    } else {
        item.raw["content"].clone()
    };
    fn cover(v: &Value, url: &str) -> bool {
        match v {
            Value::Object(m) => m.iter().any(|(k, v)| {
                (["cover", "thumb", "cover_url"].contains(&k.as_str()) && v.as_str() == Some(url))
                    || cover(v, url)
            }),
            Value::Array(a) => a.iter().any(|v| cover(v, url)),
            _ => false,
        }
    }
    fn image_field(v: &Value, source: &str) -> bool {
        match v {
            Value::Object(m) => m.iter().any(|(key, value)| {
                ([
                    "images",
                    "image",
                    "image_url",
                    "image_urls",
                    "pictures",
                    "image_list",
                    "img_src",
                    "imageUrl",
                ]
                .contains(&key.as_str())
                    && value.to_string().contains(source))
                    || image_field(value, source)
            }),
            Value::Array(values) => values.iter().any(|v| image_field(v, source)),
            _ => false,
        }
    }
    if cover(&content, &a.url) || item.raw["item"]["image"].as_str() == Some(a.url.as_str()) {
        "cover"
    } else if item.ty == "private"
        && (image || item.raw["msg_type"].as_u64() == Some(2) || image_field(&content, &a.url))
    {
        "private_image"
    } else if image {
        "content_image"
    } else {
        "message_attachment"
    }
}
pub fn from_messages(
    input: &crate::messages::Doc,
    root: &Path,
    policy: ResourcePolicy,
) -> Envelope {
    let account = input.account_uid.parse().unwrap_or(0);
    let mut doc = Envelope::new(account, "messages");
    let mut b = Resources::new(root, account, policy);
    if let Ok(saved) =
        serde_json::from_value::<BTreeMap<String, Value>>(input.archive["resources"].clone())
    {
        for (key, value) in saved {
            b.catalog.entry(key).or_insert(value);
        }
    }

    let mut related: BTreeSet<String> = input.items.iter().map(|i| i.sender_uid.clone()).collect();
    related.extend(
        input
            .sessions
            .iter()
            .filter(|s| s.session_type == 1)
            .map(|s| s.talker_id.clone()),
    );
    for item in &input.items {
        related.extend(item.like_users.iter().map(|u| u.uid.clone()));
        if let Some(users) = item.raw["users"].as_array() {
            related.extend(users.iter().map(|u| uid(&u["mid"])));
        }
    }
    if let Some(users) = input.archive["users"].as_object() {
        for (key, user) in users {
            if !related.contains(key)
                && !input
                    .items
                    .iter()
                    .any(|i| key.starts_with(&format!("anonymous:{}:", i.id)))
            {
                continue;
            }
            let photo = user["avatar_resource_id"]
                .as_str()
                .and_then(|id| input.archive["resources"][id]["url"].as_str())
                .unwrap_or_default();
            doc.user(
                key,
                user["name"].as_str().unwrap_or_default(),
                photo,
                &mut b,
            );
        }
    }
    doc.sessions = input
        .sessions
        .iter()
        .map(|s| serde_json::to_value(s).unwrap_or_default())
        .collect();
    for session in &mut doc.sessions {
        if let Some(n) = session["timestamp"].as_u64() {
            session["timestamp"] = if n == 0 {
                Value::Null
            } else {
                json!(seconds(n))
            };
        }
    }
    let profiles = crate::message_profiles::Service::default().snapshot(
        &input.account_uid,
        crate::message_profiles::local_profiles(input),
    );
    for (key, p) in profiles {
        doc.user(&key, &p.name, &p.avatar_url, &mut b);
    }
    let mut subjects = crate::message_subjects::index(input);
    for item in input.items.iter().filter(|i| i.ty == "likes") {
        let mut context = item.clone();
        context.ty = "replies".into();
        if let Some(found) = crate::message_subjects::resolve(&context) {
            subjects
                .entry(found.key.clone())
                .and_modify(|old| old.merge(&found))
                .or_insert(found);
        }
    }
    if let Some(saved) = input.archive["subjects"].as_object() {
        for (key, value) in saved {
            if let Some(local) = subjects.get_mut(key) {
                if let Ok(found) =
                    serde_json::from_value::<crate::message_subjects::Subject>(value.clone())
                {
                    local.merge(&found);
                }
            }
        }
    }

    if let Ok(path) = crate::checkpoint::data_dir() {
        if let Ok(bytes) = std::fs::read(
            path.join("cache/message-subjects")
                .join(format!("{}.json", input.account_uid)),
        ) {
            if let Ok(cached) =
                serde_json::from_slice::<BTreeMap<String, crate::message_subjects::Subject>>(&bytes)
            {
                for (key, local) in &mut subjects {
                    if let Some(saved) = cached.get(key) {
                        local.merge(saved);
                    }
                }
            }
        }
    }
    for (key, subject) in subjects {
        let mut value = serde_json::to_value(&subject).unwrap_or_default();
        value["cover_resource_id"] = json!(b.add("cover", &subject.cover, None));
        doc.subjects.insert(key, value);
    }
    for item in &input.items {
        if item.raw["user"].is_object() {
            actor(&mut doc, &mut b, &item.raw["user"], &item.sender_uid);
        }
        if let Some(users) = item.raw["users"].as_array() {
            for user in users {
                actor(&mut doc, &mut b, user, "");
            }
        }
        if !doc.users.contains_key(&item.sender_uid) {
            doc.user(&item.sender_uid, &item.sender_name, "", &mut b);
        }
        let subject = crate::message_subjects::resolve(item).map(|s| s.key);
        let resource_ids: Vec<_> = item
            .attachments
            .iter()
            .map(|a| b.add(attachment_type(item, a), &a.url, Some(&a.path)))
            .filter(|s| !s.is_empty())
            .collect();
        let mut likes = vec![];
        for (position, user) in item.like_users.iter().enumerate() {
            let user_key = if user.uid.is_empty() || user.uid == "0" {
                format!("anonymous:{}:{}:{}", item.id, user.key, position)
            } else {
                user.uid.clone()
            };
            if user_key.starts_with("anonymous:") {
                doc.users.insert(user_key.clone(),json!({"uid":null,"name":user.nickname,"avatar_resource_id":b.add("avatar",&user.avatar,None)}));
            } else {
                doc.user(&user.uid, &user.nickname, &user.avatar, &mut b);
            }
            likes.push(json!({"key":user.key,"user_key":user_key,"user_uid":user.uid,"like_time":seconds(user.like_time),"raw":user.raw}));
        }
        doc.items.push(json!({"id":item.id,"type":item.ty,"session_key":item.session,"subject_key":subject,"sender_uid":item.sender_uid,"actor_name":item.sender_name,"text":item.text,"timestamp":if item.timestamp==0{Value::Null}else{json!(seconds(item.timestamp))},"url":item.url,"native_uri":item.raw["item"]["native_uri"],"status":item.status,"like_count":item.like_count,"like_users":likes,"like_detail":item.like_detail,"resource_ids":resource_ids,"raw":item.raw}));
        if !item.my_comment_rpid.is_empty() || !item.reply_rpid.is_empty() {
            doc.items.last_mut().unwrap()["fields"] = json!({
                "my_comment_rpid":item.my_comment_rpid, "reply_rpid":item.reply_rpid
            });
        }
    }
    if let Some(session) = input.sessions.first().filter(|_| input.sessions.len() == 1) {
        doc.source["session_key"] = json!(session.key);
        doc.source["category"] = json!(if session.session_type == 2 {
            "fanclub"
        } else {
            "private"
        });
    } else {
        let types: BTreeSet<_> = input.items.iter().map(|i| i.ty.as_str()).collect();
        doc.source["categories"] = json!(types);
    }
    let mut scope = message_scope(&doc.source);
    if let Some(key) = input.archive["export_scope"].as_str() {
        scope = key.into();
    }
    if !scope.is_empty() && !scope.starts_with("user_") && !scope.starts_with("group_") {
        doc.source["category"] = json!(scope);
    }
    let known = &input.archive["collections"][&scope];
    let same_scope = !scope.is_empty() && message_scope(&input.archive["source"]) == scope;
    doc.collection = compact_collection(
        if known.is_object() {
            known
        } else if same_scope {
            &input.archive["collection"]
        } else {
            &Value::Null
        },
        doc.items.len(),
    );
    doc.collection["archived_total"] = json!(doc.items.len());
    doc.resources = b.entries;
    let mut value = serde_json::to_value(&doc).unwrap();
    string_ids(&mut value);
    serde_json::from_value(value).unwrap()
}
fn resource_url(doc: &Envelope, key: &Value) -> String {
    key.as_str()
        .and_then(|k| doc.resources.get(k))
        .and_then(|r| r["url"].as_str())
        .unwrap_or_default()
        .into()
}
fn comment_pictures(doc: &Envelope, item: &Value) -> Vec<String> {
    let mut pictures: Vec<_> = item["resource_ids"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|key| resource_url(doc, key))
        .filter(|url| !url.is_empty())
        .collect();
    for picture in crate::bili::pictures_of(&item["raw"]) {
        if !pictures.contains(&picture) {
            pictures.push(picture);
        }
    }
    pictures
}
pub fn comments(doc: &Envelope) -> Result<crate::model::ArchiveDoc> {
    doc.validate()?;
    if doc.source["type"] != "comments" {
        bail!("归档不是评论文档");
    }
    let mut output = crate::model::ArchiveDoc::default();
    let mut comments = BTreeMap::<&str, Vec<&Value>>::new();
    let mut replies = BTreeMap::<(&str, &str), Vec<&Value>>::new();
    for item in &doc.items {
        let subject = item["subject_key"].as_str().unwrap_or_default();
        match item["type"].as_str() {
            Some("comment") => comments.entry(subject).or_default().push(item),
            Some("comment_reply") => replies
                .entry((subject, item["root_id"].as_str().unwrap_or_default()))
                .or_default()
                .push(item),
            _ => {}
        }
    }
    for (key, subject) in &doc.subjects {
        let mut value = subject.clone();
        value["aid"] = subject["id"].clone();
        if subject["cover_resource_id"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
        {
            value["cover"] = json!(resource_url(doc, &subject["cover_resource_id"]));
        }
        number_ids(&mut value);
        let mut video: crate::model::Video = serde_json::from_value(value)?;
        video.comments.clear();
        for &item in comments.get(key.as_str()).into_iter().flatten() {
            let mut value = item["fields"].clone();
            value["replies"] = json!([]);
            value["rpid"] = item["id"].clone();
            value["mid"] = item["user_uid"].clone();
            value["content"] = item["text"].clone();
            value["ctime"] = json!(item["timestamp"].as_i64().unwrap_or(0));
            if let Some(user) = item["user_uid"].as_str().and_then(|key| doc.users.get(key)) {
                value["uname"] = user["name"].clone();
                value["avatar"] = json!(resource_url(doc, &user["avatar_resource_id"]));
            }
            value["pictures"] = json!(comment_pictures(doc, item));
            value["raw"] = item["raw"].clone();
            number_ids(&mut value);
            let mut c: crate::model::Comment = serde_json::from_value(value)?;
            let root = item["id"].as_str().unwrap_or_default();
            for &reply in replies.get(&(key.as_str(), root)).into_iter().flatten() {
                let mut v = reply["fields"].clone();
                v["rpid"] = reply["id"].clone();
                v["mid"] = reply["user_uid"].clone();
                v["content"] = reply["text"].clone();
                v["ctime"] = json!(reply["timestamp"].as_i64().unwrap_or(0));
                v["parent"] = reply["parent_id"].clone();
                if let Some(user) = reply["user_uid"]
                    .as_str()
                    .and_then(|key| doc.users.get(key))
                {
                    v["uname"] = user["name"].clone();
                    v["avatar"] = json!(resource_url(doc, &user["avatar_resource_id"]));
                }
                v["pictures"] = json!(comment_pictures(doc, reply));
                v["raw"] = reply["raw"].clone();
                number_ids(&mut v);
                c.replies.push(serde_json::from_value(v)?);
            }
            video.comments.push(c);
        }
        output.videos.push(video);
    }
    output.meta.exported_at = doc.meta["exported_at"].as_str().unwrap_or_default().into();
    output.archive = metadata(doc);
    output.recount();
    Ok(output)
}
pub fn messages(doc: &Envelope) -> Result<crate::messages::Doc> {
    doc.validate()?;
    if doc.source["type"] != "messages" {
        bail!("归档不是账号消息文档");
    }
    let account = doc.meta["account_uid"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("消息归档缺少账号"))?;
    let mut output = crate::messages::Doc::new(account);
    output.sessions = doc
        .sessions
        .iter()
        .map(|s| {
            let mut value = s.clone();
            if value["timestamp"].is_null() {
                value["timestamp"] = json!(0);
            }
            serde_json::from_value(value)
        })
        .collect::<std::result::Result<_, _>>()?;
    for item in &doc.items {
        let user = doc
            .users
            .get(item["sender_uid"].as_str().unwrap_or_default());
        let mut likes = vec![];
        if let Some(users) = item["like_users"].as_array() {
            for u in users {
                let known = doc.users.get(
                    u["user_key"]
                        .as_str()
                        .or_else(|| u["user_uid"].as_str())
                        .unwrap_or_default(),
                );
                likes.push(crate::messages::LikeUser {
                    key: u["key"].as_str().unwrap_or_default().into(),
                    uid: uid(&u["user_uid"]),
                    nickname: known
                        .and_then(|v| v["name"].as_str())
                        .unwrap_or_default()
                        .into(),
                    avatar: known
                        .map(|v| resource_url(doc, &v["avatar_resource_id"]))
                        .unwrap_or_default(),
                    like_time: u["like_time"].as_u64().unwrap_or(0),
                    raw: u["raw"].clone(),
                });
            }
        }
        let attachments = item["resource_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|id| id.as_str().and_then(|k| doc.resources.get(k)))
            .map(|r| crate::messages::Attachment {
                id: r["id"].as_str().unwrap_or_default().into(),
                url: r["url"].as_str().unwrap_or_default().into(),
                path: r["path"].as_str().unwrap_or_default().into(),
                mime: r["mime"].as_str().unwrap_or_default().into(),
                error: r["error"].as_str().unwrap_or_default().into(),
            })
            .collect();
        output.items.push(crate::messages::Item {
            id: uid(&item["id"]),
            ty: item["type"].as_str().unwrap_or_default().into(),
            session: item["session_key"].as_str().unwrap_or_default().into(),
            sender_uid: uid(&item["sender_uid"]),
            sender_name: user
                .and_then(|v| v["name"].as_str())
                .or_else(|| item["actor_name"].as_str())
                .unwrap_or_default()
                .into(),
            text: item["text"].as_str().unwrap_or_default().into(),
            timestamp: item["timestamp"].as_u64().unwrap_or(0),
            url: item["url"].as_str().unwrap_or_default().into(),
            status: item["status"].as_u64().unwrap_or(0) as u32,
            attachments,
            like_count: item["like_count"].as_u64().unwrap_or(0),
            like_users: likes,
            like_detail: serde_json::from_value(item["like_detail"].clone()).unwrap_or_default(),
            raw: item["raw"].clone(),
            my_comment_rpid: crate::my_comments::numeric_id(&item["fields"]["my_comment_rpid"]),
            reply_rpid: crate::my_comments::numeric_id(&item["fields"]["reply_rpid"]),
        });
    }
    output.exported_at = doc.meta["exported_at"].as_str().unwrap_or_default().into();
    output.archive = metadata(doc);
    let scope = message_scope(&doc.source);
    if !scope.is_empty() {
        output.archive["collections"][&scope] =
            compact_collection(&doc.collection, doc.items.len());
    }
    output.validate(account)?;
    Ok(output)
}
pub fn write(path: &Path, doc: &Envelope) -> Result<()> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let path = absolute.as_path();
    doc.validate()?;
    if path.exists() {
        Envelope::parse(&std::fs::read(path)?)?;
    }
    crate::secure::write_config(path, &serde_json::to_value(doc)?)?;
    register(path, doc)?;
    Ok(())
}
fn register(path: &Path, doc: &Envelope) -> Result<()> {
    let base = storage_root(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    );
    if base.file_name().is_none_or(|s| s != "archive-v1") {
        return Ok(());
    }
    let rel = path
        .strip_prefix(&base)?
        .to_string_lossy()
        .replace('\\', "/");
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap();
    let file = base.join("manifest.json");
    let mut value: Value = if file.exists() {
        serde_json::from_slice(&std::fs::read(&file)?)?
    } else {
        json!({"format":"bili-archive-manifest","version":1,"documents":{}})
    };
    if value["format"] != "bili-archive-manifest"
        || value["version"] != 1
        || !value["documents"].is_object()
    {
        bail!("归档清单无效，未覆盖");
    }
    value["documents"][rel] =
        json!({"type":doc.source["type"],"account_uid":doc.meta["account_uid"]});
    crate::secure::write_config(&file, &value)
}
pub fn resource_counts(doc: &Envelope) -> (usize, usize) {
    let saved = doc
        .resources
        .values()
        .filter(|r| r["status"] == "saved")
        .count();
    let policy = ResourcePolicy::load();
    let pending = doc
        .resources
        .values()
        .filter(|r| r["required"] == true || policy.enabled(r["type"].as_str().unwrap_or_default()))
        .filter(|r| r["status"] != "saved")
        .count();
    (saved, pending)
}
pub fn process_resources(
    doc: &mut Envelope,
    root: &Path,
    cancel: &AtomicBool,
    download: impl FnMut(&str, &Path) -> Result<()>,
    mut progress: impl FnMut(usize, usize, usize, usize, &str),
) -> Result<()> {
    process_resources_observed(
        doc,
        root,
        cancel,
        download,
        |n, total, saved, failed, key| {
            progress(n, total, saved, failed, key);
            Ok(())
        },
    )
}
pub fn process_resources_observed(
    doc: &mut Envelope,
    root: &Path,
    cancel: &AtomicBool,
    mut download: impl FnMut(&str, &Path) -> Result<()>,
    mut progress: impl FnMut(usize, usize, usize, usize, &str) -> Result<()>,
) -> Result<()> {
    let keys: Vec<_> = doc
        .resources
        .iter()
        .filter(|(_, r)| r["status"] == "pending" || r["status"] == "failed")
        .map(|(k, _)| k.clone())
        .collect();
    let total = keys.len();
    let (mut saved, mut failed) = (0, 0);
    for (index, key) in keys.into_iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        progress(index, total, saved, failed, &key)?;
        let res = doc.resources.get_mut(&key).unwrap();
        let source = res["url"].as_str().unwrap_or_default().to_owned();
        let account = doc.meta["account_uid"].as_str().unwrap_or("public");
        let rel = resource_path(account, &key, &source);
        let dest = root.join(&rel);
        std::fs::create_dir_all(dest.parent().unwrap())?;
        if !dest
            .parent()
            .unwrap()
            .canonicalize()?
            .starts_with(root.canonicalize()?)
        {
            bail!("目标资源链接越界");
        }
        if dest.exists() && resource_file(root, &rel).is_none() {
            bail!("目标资源链接越界");
        }
        let result = if std::fs::metadata(&dest).is_ok_and(|m| m.len() > 0) {
            Ok(())
        } else {
            download(&source, &dest)
        };
        if cancel.load(Ordering::SeqCst) && result.is_err() {
            break;
        }
        match result {
            Ok(()) => {
                saved += 1;
                let m = std::fs::metadata(&dest)?;
                res["status"] = json!("saved");
                res["path"] = json!(rel);
                res["size"] = json!(m.len());
                res["mime"] = json!(crate::messages::media_type(&dest));
                res["checksum"] = json!(file_hash(&dest)?);
                res["error"] = json!("");
            }
            Err(e) => {
                failed += 1;
                res["status"] = json!("failed");
                res["error"] = json!(e.to_string());
            }
        }
        let index_path = root.join("resources").join(account).join("index.json");
        let mut index_value: Value = if index_path.exists() {
            serde_json::from_slice(&std::fs::read(&index_path)?)?
        } else {
            json!({})
        };
        index_value[&key] = res.clone();
        crate::secure::write_config(&index_path, &index_value)?;
        progress(index + 1, total, saved, failed, &key)?;
    }
    Ok(())
}
pub(crate) fn file_hash(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut digest = Md5::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        digest.update(&buf[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
pub fn copy_resources(doc: &mut Envelope, source: &Path, dest: &Path) -> Result<()> {
    let mut batch = ResourceCopies::default();
    batch.copy(doc, source, dest, false)?;
    batch.finish()
}

/// Reuse disk checks and write each account resource index once per import.
#[derive(Default)]
pub struct ResourceCopies {
    hashes: BTreeMap<PathBuf, String>,
    indexes: BTreeMap<PathBuf, Value>,
}

impl ResourceCopies {
    fn hash(&mut self, path: &Path) -> Result<&str> {
        use std::collections::btree_map::Entry;
        Ok(match self.hashes.entry(path.to_path_buf()) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(file_hash(path)?),
        })
    }

    /// Returns whether a resource reference in the document needed normalization.
    pub fn copy(&mut self, doc: &mut Envelope, source: &Path, dest: &Path, staged_source: bool) -> Result<bool> {
        let mut changed = false;
        for res in doc.resources.values_mut() {
            let rel = res["path"].as_str().unwrap_or_default().to_owned();
            if rel.is_empty() {
                continue;
            }
            if !safe_path(&rel) {
                bail!("资源路径越界");
            }
            let old = source.join(&rel);
            let new = dest.join(&rel);
            if !self.hashes.contains_key(&old) {
                if !old.is_file() || std::fs::metadata(&old).is_ok_and(|meta| meta.len() == 0) {
                    res["path"] = json!("");
                    res["status"] = json!(if res["required"] == true { "pending" } else { "not_requested" });
                    res["error"] = json!("导入包资源缺失");
                    changed = true;
                    continue;
                }
                if resource_file(source, &rel).is_none() {
                    bail!("资源文件链接越界");
                }
            }
            let hash = self.hash(&old)?.to_owned();
            let declared = res["checksum"].as_str().unwrap_or_default();
            anyhow::ensure!(declared.is_empty() || declared == hash, "资源校验不一致：{}", rel);
            if new.exists() {
                if !self.hashes.contains_key(&new) && resource_file(dest, &rel).is_none() {
                    bail!("目标资源链接越界");
                }
                anyhow::ensure!(self.hash(&new)? == hash, "目标资源已存在但内容不一致，未覆盖");
            } else if old != new {
                let parent = new.parent().context("资源路径缺少目录")?;
                std::fs::create_dir_all(parent)?;
                anyhow::ensure!(
                    parent.canonicalize()?.starts_with(dest.canonicalize()?),
                    "目标资源链接越界"
                );
                // Both paths belong to the import's private staging directory.
                if !staged_source || std::fs::hard_link(&old, &new).is_err() {
                    std::fs::copy(&old, &new)?;
                }
                self.hashes.insert(new, hash.clone());
            }
            if declared != hash {
                changed = true;
                res["checksum"] = json!(hash);
            }
            if let Some(account) = doc.meta["account_uid"].as_str() {
                let index_path = dest.join("resources").join(account).join("index.json");
                let index = self.indexes.entry(index_path.clone()).or_insert_with(|| {
                    std::fs::read(&index_path)
                        .ok()
                        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                        .filter(Value::is_object)
                        .unwrap_or_else(|| json!({}))
                });
                index[res["id"].as_str().unwrap_or_default()] = res.clone();
            }
        }
        Ok(changed)
    }

    pub fn finish(self) -> Result<()> {
        for (path, index) in self.indexes {
            crate::secure::write_config(&path, &index)?;
        }
        Ok(())
    }
}
/// Resolve only files under the declared root, including when existing directories are symlinks.
pub fn resource_file(base: &Path, relative: &str) -> Option<PathBuf> {
    if !safe_path(relative) || !relative.starts_with("resources/") {
        return None;
    }
    let canonical = base.canonicalize().ok()?;
    let file = base.join(relative).canonicalize().ok()?;
    (file.starts_with(canonical) && file.is_file()).then_some(file)
}
pub fn message_resources(
    input: &crate::messages::Doc,
    root: &Path,
    policy: ResourcePolicy,
) -> BTreeMap<String, Value> {
    let mut b = Resources::new(root, input.account_uid.parse().unwrap_or(0), policy);
    collect_message_resources(&mut b, input);
    b.entries
}

/// Aggregate preview resource status without hashing media or reopening the index per session.
pub(crate) struct MessageResourceScan(Resources);

impl MessageResourceScan {
    pub fn new(root: &Path, uid: u64) -> Self {
        let mut resources = Resources::new(root, uid, ResourcePolicy::load());
        resources.verify_checksums = false;
        Self(resources)
    }

    pub fn add(&mut self, doc: &crate::messages::Doc) {
        collect_message_resources(&mut self.0, doc);
    }

    pub fn finish(self) -> BTreeMap<String, Value> { self.0.entries }
}

fn collect_message_resources(b: &mut Resources, input: &crate::messages::Doc) {
    if let Some(saved) = input.archive["resources"].as_object() {
        for (key, value) in saved {
            b.catalog.insert(key.clone(), value.clone());
            let ty = value["type"].as_str().unwrap_or("message_attachment");
            b.add(
                ty,
                value["url"].as_str().unwrap_or_default(),
                value["path"].as_str(),
            );
        }
    }
    for item in &input.items {
        for a in &item.attachments {
            b.add(attachment_type(item, a), &a.url, Some(&a.path));
        }
        let mut users = vec![&item.raw["user"]];
        if let Some(values) = item.raw["users"].as_array() {
            users.extend(values);
        }
        for user in users {
            for key in ["avatar", "face"] {
                if let Some(photo) = user[key].as_str() {
                    b.add("avatar", photo, None);
                }
            }
        }
        for user in &item.like_users {
            b.add("avatar", &user.avatar, None);
        }
    }
    for profile in crate::message_profiles::local_profiles(input).values() {
        b.add("avatar", &profile.avatar_url, None);
    }
    for subject in crate::message_subjects::index(input).values() {
        b.add("cover", &subject.cover, None);
    }
}
pub fn message_resource_counts(input: &crate::messages::Doc) -> (usize, usize) {
    let values = message_resources(input, &configured_root(), ResourcePolicy::load());
    (
        values.values().filter(|r| r["status"] == "saved").count(),
        values
            .values()
            .filter(|r| r["status"] == "pending" || r["status"] == "failed")
            .count(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn comments_and_replies_round_trip_with_string_ids_and_raw() {
        let root = tempfile::tempdir().unwrap();
        let mut input = crate::model::ArchiveDoc::default();
        for (ty, content_type) in [
            (1, "video"),
            (11, "dynamic"),
            (17, "dynamic"),
            (12, "article"),
            (14, "audio"),
        ] {
            input.videos.push(crate::model::Video {
                aid: ty as u64,
                owner_mid: 12,
                r#type: content_type.into(),
                comment_type: ty,
                title: "标题".into(),
                comments: vec![crate::model::Comment {
                    rpid: u64::MAX - 1,
                    mid: u64::MAX - 2,
                    content: "评论".into(),
                    raw: json!({"rpid":u64::MAX-1}),
                    replies: vec![crate::model::Reply {
                        rpid: u64::MAX - 3,
                        mid: 2,
                        parent: u64::MAX - 1,
                        content: "回复".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            });
        }
        let doc = from_comments(&input, 12, root.path(), ResourcePolicy::default());
        doc.validate().unwrap();
        let saved = serde_json::to_value(&doc).unwrap();
        assert_eq!(saved["items"][0]["id"], (u64::MAX - 1).to_string());
        assert_eq!(saved["items"][0]["raw"]["rpid"], (u64::MAX - 1).to_string());
        let loaded =
            comments(&Envelope::parse(&serde_json::to_vec(&doc).unwrap()).unwrap()).unwrap();
        assert_eq!(loaded.videos.len(), 5);
        assert_eq!(loaded.videos[0].comments[0].rpid, u64::MAX - 1);
        assert_eq!(loaded.videos[0].comments[0].replies[0].parent, u64::MAX - 1);
    }
    fn private_doc() -> crate::messages::Doc {
        let image = "https://example.test/private.png";
        let voice = "https://example.test/voice.mp3";
        let mut input = crate::messages::Doc::new(12);
        input.sessions.push(crate::messages::Session {
            key: "group_4".into(),
            talker_id: "4".into(),
            session_type: 2,
            name: "粉丝团".into(),
            ..Default::default()
        });
        input.items.push(crate::messages::Item{id:"1".into(),ty:"private".into(),session:"group_4".into(),sender_uid:"2".into(),sender_name:"成员".into(),text:"图片".into(),timestamp:1700000000000,raw:json!({"msg_type":2,"content":{"url":image},"user":{"mid":"2","avatar":"https://example.test/avatar.jpg"}}),attachments:vec![crate::messages::Attachment{id:hash(image),url:image.into(),..Default::default()}],..Default::default()});
        input.items.push(crate::messages::Item {
            id: "2".into(),
            ty: "private".into(),
            session: "group_4".into(),
            sender_uid: "2".into(),
            raw: json!({"msg_type":3}),
            attachments: vec![crate::messages::Attachment {
                id: hash(voice),
                url: voice.into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        input
    }
    #[test]
    fn private_images_are_mandatory_and_bundles_verify_integrity() {
        let root = tempfile::tempdir().unwrap();
        let input = private_doc();
        let mut doc = from_messages(&input, root.path(), ResourcePolicy::default());
        let image = hash("https://example.test/private.png");
        let voice = hash("https://example.test/voice.mp3");
        assert_eq!(doc.resources[&image]["status"], "pending");
        assert_eq!(doc.resources[&voice]["status"], "pending");
        assert_eq!(doc.items[0]["timestamp"], 1700000000u64);
        let cancel = AtomicBool::new(false);
        let mut calls = 0;
        process_resources(
            &mut doc,
            root.path(),
            &cancel,
            |_, path| {
                calls += 1;
                std::fs::write(path, b"\x89PNG\r\n\x1a\nexample")?;
                Ok(())
            },
            |_, _, _, _, _| {},
        )
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(doc.resources[&image]["status"], "saved");
        let bundle = tempfile::tempdir().unwrap();
        copy_resources(&mut doc, root.path(), bundle.path()).unwrap();
        let loaded = messages(&doc).unwrap();
        assert_eq!(loaded.sessions[0].session_type, 2);
        assert!(!loaded.items[0].attachments[0].path.is_empty());
        let file = bundle
            .path()
            .join(doc.resources[&image]["path"].as_str().unwrap());
        std::fs::write(file, b"damaged").unwrap();
        let next = tempfile::tempdir().unwrap();
        assert!(copy_resources(&mut doc, bundle.path(), next.path()).is_err());
    }
    #[test]
    fn cancelled_and_failed_resources_remain_resumable() {
        let root = tempfile::tempdir().unwrap();
        let mut doc = from_messages(&private_doc(), root.path(), ResourcePolicy::default());
        let cancel = AtomicBool::new(false);
        process_resources(
            &mut doc,
            root.path(),
            &cancel,
            |_, _| bail!("offline"),
            |_, _, _, _, _| {},
        )
        .unwrap();
        assert!(doc.resources.values().any(|r| r["status"] == "failed"));
        let retry = from_messages(&private_doc(), root.path(), ResourcePolicy::default());
        assert!(retry.resources.values().any(|r| r["error"] == "offline"));
        cancel.store(true, Ordering::SeqCst);
        process_resources(
            &mut doc,
            root.path(),
            &cancel,
            |_, _| panic!("cancelled"),
            |_, _, _, _, _| {},
        )
        .unwrap();
    }
    #[test]
    fn archive_metadata_does_not_depend_on_ui_caches() {
        let root = tempfile::tempdir().unwrap();
        let mut doc = from_messages(&private_doc(), root.path(), ResourcePolicy::default());
        doc.users.get_mut("2").unwrap()["name"] = json!("持久昵称");
        let loaded = messages(&doc).unwrap();
        assert_eq!(loaded.items[0].sender_name, "持久昵称");
        let next = from_messages(&loaded, root.path(), ResourcePolicy::default());
        assert_eq!(next.users["2"]["name"], "持久昵称");
    }
    #[test]
    fn invalid_references_and_paths_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let mut doc = from_messages(&private_doc(), root.path(), ResourcePolicy::default());
        doc.items[0]["resource_ids"] = json!(["unknown"]);
        assert!(doc.validate().is_err());
        assert!(!safe_path("../secret"));
        assert!(!safe_path("C:\\secret"));
        assert!(!safe_path("resources/12/../../secret"));
        let policy = ResourcePolicy::default();
        assert!(policy.enabled("private_image"));
        assert!(!policy.enabled("avatar"));
        assert!(!policy.enabled("cover"));
        assert!(!policy.enabled("content_image"));
        assert!(policy.enabled("message_attachment"));
        let old: ResourcePolicy =
            serde_json::from_value(json!({"other_attachments":false})).unwrap();
        assert!(old.enabled("message_attachment"));
        assert!(serde_json::to_value(old)
            .unwrap()
            .get("other_attachments")
            .is_none());
    }
}

pub fn comment_root(doc: &crate::model::ArchiveDoc) -> PathBuf {
    doc.archive["resource_root"]
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_else(configured_root)
}
pub fn comment_account(doc: &crate::model::ArchiveDoc) -> u64 {
    doc.archive["meta"]["account_uid"]
        .as_str()
        .or_else(|| doc.archive["account_uid"].as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(account)
}
