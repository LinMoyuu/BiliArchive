//! Canonical message subjects and a separate, account-scoped presentation cache.
use crate::{
    messages::{Doc, Item},
    model::ArchiveDoc,
};
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Condvar, Mutex},
    time::Duration,
};

const ALPHABET: &[u8] = b"FcwAPNKTMug3GV5Lj7EJnHpWsx4tb8haYeviqBz6rkCy12mUSDQX9RdoZf";
const XOR: u64 = 23442827791579;
const MAX: u64 = 1 << 51;
// Encoding reference: https://github.com/Colerar/abv/blob/main/src/lib.rs
pub fn av_to_bv(aid: u64) -> Option<String> {
    if aid == 0 || aid >= MAX {
        return None;
    }
    let mut bytes = *b"BV1000000000";
    let mut n = (MAX | aid) ^ XOR;
    for at in (3..12).rev() {
        bytes[at] = ALPHABET[(n % 58) as usize];
        n /= 58;
    }
    bytes.swap(3, 9);
    bytes.swap(4, 7);
    String::from_utf8(bytes.to_vec()).ok()
}
fn bv_to_av(bv: &str) -> Option<String> {
    if bv.len() != 12 || !bv.starts_with("BV1") {
        return None;
    }
    let mut bytes = bv.as_bytes().to_vec();
    bytes.swap(3, 9);
    bytes.swap(4, 7);
    let mut n = 0u64;
    for b in &bytes[3..] {
        n = n
            .checked_mul(58)?
            .checked_add(ALPHABET.iter().position(|v| v == b)? as u64)?;
    }
    let aid = (n & (MAX - 1)) ^ XOR;
    (av_to_bv(aid).as_deref() == Some(bv)).then(|| aid.to_string())
}
fn positive(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let s = s.trim_start_matches('0');
    (!s.is_empty()).then(|| s.to_owned())
}
fn id(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => positive(s),
        Value::Number(n) => positive(&n.to_string()),
        _ => None,
    }
}
fn text(v: &Value) -> String {
    v.as_str().unwrap_or("").trim().to_owned()
}
#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub struct Subject {
    pub key: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub id: String,
    pub title: String,
    pub text: String,
    pub bvid: String,
    pub url: String,
    #[serde(default)]
    pub cover: String,
    #[serde(default)]
    pub cover_src: String,
    #[serde(default)]
    pub cover_checked: bool,
    #[serde(default)]
    pub error: String,
}
impl Subject {
    pub fn missing(&self) -> bool {
        match self.ty.as_str() {
            "video" => {
                self.title.is_empty()
                    || self.bvid.is_empty()
                    || (!self.cover_checked && self.cover.is_empty())
            }
            "dynamic" => self.text.is_empty() || (!self.cover_checked && self.cover.is_empty()),
            _ => false,
        }
    }
    pub fn merge(&mut self, other: &Self) {
        if self.key != other.key {
            return;
        }
        if self.title.is_empty() {
            self.title = other.title.clone();
        }
        if self.text.is_empty() {
            self.text = other.text.clone();
        }
        if self.cover.is_empty() {
            self.cover = other.cover.clone();
            self.cover_src = other.cover_src.clone();
        } else if self.cover == other.cover
            && (self.cover_src.is_empty()
                || other.cover_src.starts_with("/api/messages/attachments/"))
        {
            self.cover_src = other.cover_src.clone();
        }
        self.cover_checked |= other.cover_checked || !self.cover.is_empty();
        if self.bvid.is_empty() {
            self.bvid = other.bvid.clone();
        }
    }
}
fn target(ty: &str, value: &str) -> Option<Subject> {
    let ident = if ty == "video" && value.starts_with("BV") {
        bv_to_av(value)?
    } else {
        positive(value)?
    };
    let (key, url) = match ty {
        "video" => (
            format!("video:aid:{ident}"),
            format!("https://www.bilibili.com/video/av{ident}"),
        ),
        "dynamic" => (
            format!("dynamic:{ident}"),
            format!("https://t.bilibili.com/{ident}"),
        ),
        "article" => (
            format!("article:{ident}"),
            format!("https://www.bilibili.com/read/cv{ident}"),
        ),
        _ => return None,
    };
    let bvid = if ty == "video" {
        ident.parse().ok().and_then(av_to_bv).unwrap_or_default()
    } else {
        String::new()
    };
    Some(Subject {
        key,
        ty: ty.into(),
        id: ident,
        bvid,
        url,
        ..Default::default()
    })
}
fn from_url(raw: &str) -> Option<Subject> {
    let raw = if raw.starts_with("//") {
        format!("https:{raw}")
    } else {
        raw.to_owned()
    };
    let u = reqwest::Url::parse(&raw).ok()?;
    let parts: Vec<_> = u.path().trim_matches('/').split('/').collect();
    let host = u.host_str()?;
    if matches!(u.scheme(), "http" | "https")
        && matches!(
            host,
            "www.bilibili.com" | "m.bilibili.com" | "bilibili.com" | "t.bilibili.com"
        )
    {
        return match parts.as_slice() {
            ["video", v] => target("video", v.strip_prefix("av").unwrap_or(v)),
            ["opus", v] => target("dynamic", v),
            [v] if host == "t.bilibili.com" => target("dynamic", v),
            ["read", v] => target("article", v.strip_prefix("cv")?),
            _ => None,
        };
    }
    if u.scheme() == "bilibili" {
        return match (host, parts.as_slice()) {
            ("video", [v]) => target("video", v),
            ("dynamic" | "opus", [v]) => target("dynamic", v),
            ("article", [v]) => target("article", v),
            ("following", ["detail", v]) => target("dynamic", v),
            ("following", ["detail"]) => u
                .query_pairs()
                .find(|(k, _)| k == "dynamic_id")
                .and_then(|(_, v)| target("dynamic", &v)),
            _ => None,
        };
    }
    None
}
pub fn image_url(raw: &str) -> String {
    let raw = raw.trim();
    let raw = if raw.starts_with("//") {
        format!("https:{raw}")
    } else {
        raw.into()
    };
    match reqwest::Url::parse(&raw) {
        Ok(url) if matches!(url.scheme(), "http" | "https") && url.host_str().is_some() => {
            url.to_string()
        }
        _ => String::new(),
    }
}
fn set_cover(subject: &mut Subject, raw: &str) {
    subject.cover = image_url(raw);
    subject.cover_src = subject.cover.clone();
    if !subject.cover.is_empty() {
        subject.cover_checked = true;
    }
}
pub fn resolve(item: &Item) -> Option<Subject> {
    if !matches!(item.ty.as_str(), "replies" | "mentions") {
        return None;
    }
    let raw = &item.raw["item"];
    let mut found: Option<Subject> = None;
    let mut add = |subject: Option<Subject>| -> Option<()> {
        if let Some(s) = subject {
            if let Some(old) = &mut found {
                if old.key != s.key {
                    return None;
                }
                old.merge(&s);
            } else {
                found = Some(s);
            }
        }
        Some(())
    };
    for value in [
        item.url.as_str(),
        raw["uri"].as_str().unwrap_or(""),
        raw["native_uri"].as_str().unwrap_or(""),
    ] {
        add(from_url(value))?;
    }
    // subject_id is an album/comment OID for some feed types, not a dynamic ID.
    // Only use it directly when its namespace is explicit and unambiguous.
    if raw["type"].as_str() == Some("video") {
        add(id(&raw["subject_id"]).and_then(|v| target("video", &v)))?;
    }
    if let Some(v) = raw["bvid"].as_str() {
        add(target("video", v))?;
    }
    if let Some(v) = id(&raw["dynamic_id"]) {
        add(target("dynamic", &v))?;
    }
    let mut s = found?;
    set_cover(&mut s, raw["image"].as_str().unwrap_or(""));
    if !s.cover.is_empty() {
        if let Some(a) = item.attachments.iter().find(|a| {
            !a.path.is_empty()
                && a.id.len() == 32
                && a.id.bytes().all(|b| b.is_ascii_hexdigit())
                && image_url(&a.url) == s.cover
        }) {
            s.cover_src = format!("/api/messages/attachments/{}", a.id);
        }
    }
    match raw["type"].as_str() {
        Some("video") if s.ty == "video" => s.title = text(&raw["title"]),
        Some("dynamic" | "album") if s.ty == "dynamic" => {
            s.text = text(&raw["title"]);
            if s.text.is_empty() {
                s.text = text(&raw["desc"]);
            }
        }
        Some("article") if s.ty == "article" => s.title = text(&raw["title"]),
        _ => (),
    }
    Some(s)
}
pub fn index(doc: &Doc) -> BTreeMap<String, Subject> {
    let mut all: BTreeMap<String, Subject> = BTreeMap::new();
    for row in &doc.items {
        if let Some(s) = resolve(row) {
            all.entry(s.key.clone())
                .and_modify(|old| old.merge(&s))
                .or_insert(s);
        }
    }
    if let Some(subjects) = doc.archive["subjects"].as_object() {
        for (key, value) in subjects {
            if let Some(local) = all.get_mut(key) {
                if let Ok(saved) = serde_json::from_value::<Subject>(value.clone()) {
                    local.merge(&saved);
                }
            }
        }
    }
    all
}
pub fn summary(item: &Item) -> Value {
    let mut value = crate::messages::item_summary(item);
    if let Some(subject) = resolve(item) {
        value["subject"] = json!(subject);
    }
    value
}
pub fn local_subjects(messages: &Doc, archive: &ArchiveDoc) -> BTreeMap<String, Subject> {
    let mut all = index(messages);
    for v in &archive.videos {
        let s = match v.r#type.as_str() {
            "video" => target("video", &v.aid.to_string()),
            "dynamic" if v.dynamic_id > 0 => target("dynamic", &v.dynamic_id.to_string()),
            _ => None,
        };
        if let Some(mut s) = s {
            set_cover(
                &mut s,
                if v.cover.is_empty() {
                    v.text_pics.first().map(String::as_str).unwrap_or("")
                } else {
                    &v.cover
                },
            );
            s.title = v.title.clone();
            s.text = v.text.clone();
            if let Some(old) = all.get_mut(&s.key) {
                old.merge(&s);
            }
        }
    }
    all
}

#[derive(Default)]
struct Cache {
    loaded: BTreeSet<String>,
    accounts: BTreeMap<String, BTreeMap<String, Subject>>,
    errors: BTreeMap<(String, String), String>,
    blocked: BTreeSet<String>,
    active: BTreeSet<(String, String)>,
}
#[derive(Default)]
pub struct Service {
    cache: Mutex<Cache>,
    changed: Condvar,
}
pub type Credentials = std::sync::Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;
fn cache_file(uid: &str) -> Result<std::path::PathBuf> {
    if positive(uid).as_deref() != Some(uid) {
        bail!("invalid account");
    }
    Ok(crate::checkpoint::data_dir()?
        .join("cache/message-subjects")
        .join(format!("{uid}.json")))
}
impl Service {
    pub fn lookup(
        &self,
        uid: &str,
        local: Subject,
        retry: bool,
        credentials: &Credentials,
    ) -> Subject {
        self.lookup_with(uid, local, retry, |s| {
            credentials(uid)
                .ok_or_else(|| anyhow!("账号已切换或未登录"))
                .and_then(|cookie| fetch_subject(s, &cookie))
        })
    }
    fn lookup_with(
        &self,
        uid: &str,
        mut local: Subject,
        retry: bool,
        fetch: impl FnOnce(&Subject) -> Result<Subject>,
    ) -> Subject {
        let task = (uid.to_owned(), local.key.clone());
        let mut state = self.cache.lock().unwrap();
        if state.loaded.insert(uid.into()) {
            if let Ok(path) = cache_file(uid) {
                if let Ok(bytes) = std::fs::read(path) {
                    if let Ok(saved) = serde_json::from_slice::<BTreeMap<String, Subject>>(&bytes) {
                        state.accounts.insert(uid.into(), saved);
                    }
                }
            }
        }
        if retry {
            state.errors.remove(&task);
            state.blocked.remove(uid);
        }
        loop {
            if let Some(saved) = state.accounts.get(uid).and_then(|a| a.get(&local.key)) {
                local.merge(saved);
            }
            if !local.missing() {
                local.error.clear();
                return local;
            }
            if let Some(error) = state.errors.get(&task) {
                local.error = error.clone();
                return local;
            }
            if state.blocked.contains(uid) {
                local.error = "主题补全遇到风控，已暂停；稍后可手动重试".into();
                return local;
            }
            if state.active.len() < 2 && !state.active.contains(&task) {
                break;
            }
            state = self.changed.wait(state).unwrap();
        }
        state.active.insert(task.clone());
        drop(state);
        let result = fetch(&local);
        let mut state = self.cache.lock().unwrap();
        state.active.remove(&task);
        match result {
            Ok(found) => {
                local.merge(&found);
                if local.missing() {
                    local.error = "主题信息不完整，可稍后重试".into();
                    state.errors.insert(task, local.error.clone());
                }
                state
                    .accounts
                    .entry(uid.into())
                    .or_default()
                    .insert(local.key.clone(), local.clone());
                if let Ok(path) = cache_file(uid) {
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    if let Ok(bytes) = serde_json::to_vec(&state.accounts[uid]) {
                        let tmp = path.with_extension("tmp");
                        if std::fs::write(&tmp, bytes).is_ok() {
                            let _ = std::fs::rename(tmp, path);
                        }
                    }
                }
            }
            Err(error) => {
                let error = error.to_string();
                if ["-352", "-412", "-509", "HTTP 412", "HTTP 429"]
                    .iter()
                    .any(|v| error.contains(v))
                {
                    state.blocked.insert(uid.into());
                }
                local.error = if state.blocked.contains(uid) {
                    "主题补全遇到风控，已暂停；稍后可手动重试"
                } else {
                    "主题内容暂不可用，可稍后重试"
                }
                .into();
                state.errors.insert(task, local.error.clone());
            }
        }
        self.changed.notify_all();
        local
    }
}
fn fetch_subject(subject: &Subject, cookie: &str) -> Result<Subject> {
    let url = match subject.ty.as_str() {
        "video" => format!(
            "https://api.bilibili.com/x/web-interface/view?aid={}",
            subject.id
        ),
        "dynamic" => format!(
            "https://api.bilibili.com/x/polymer/web-dynamic/v1/detail?timezone_offset=-480&id={}",
            subject.id
        ),
        _ => bail!("unsupported subject"),
    };
    let cookie = if cookie.contains('=') {
        cookie.to_owned()
    } else {
        format!("SESSDATA={cookie}")
    };
    let response = crate::bili::http_client()?
        .get(url)
        .timeout(Duration::from_secs(10))
        .header("Referer", "https://www.bilibili.com/")
        .header("Cookie", cookie)
        .send()?;
    if !response.status().is_success() {
        bail!("HTTP {}", response.status().as_u16());
    }
    let value: Value = response.json()?;
    let code = value["code"]
        .as_i64()
        .ok_or_else(|| anyhow!("missing API code"))?;
    if code != 0 {
        bail!("API code {code}");
    }
    let mut found = subject.clone();
    found.error.clear();
    if subject.ty == "video" {
        if id(&value["data"]["aid"]).as_deref() != Some(subject.id.as_str()) {
            bail!("subject mismatch");
        }
        set_cover(&mut found, value["data"]["pic"].as_str().unwrap_or(""));
        found.title = text(&value["data"]["title"]);
        if let Some(bv) = value["data"]["bvid"].as_str() {
            if bv_to_av(bv).as_deref() == Some(subject.id.as_str()) {
                found.bvid = bv.into();
            }
        }
    } else {
        let item = &value["data"]["item"];
        if id(&item["id_str"]).as_deref() != Some(subject.id.as_str()) {
            bail!("subject mismatch");
        }
        let d = &item["modules"]["module_dynamic"];
        if !d.is_object() {
            bail!("missing dynamic content");
        }
        let major = &d["major"];
        let cover = [
            &major["opus"]["pics"][0]["url"],
            &major["opus"]["pics"][0]["src"],
            &major["draw"]["items"][0]["src"],
            &major["archive"]["cover"],
            &major["article"]["covers"][0],
        ]
        .into_iter()
        .filter_map(Value::as_str)
        .map(image_url)
        .find(|s| !s.is_empty())
        .unwrap_or_default();
        set_cover(&mut found, &cover);
        found.text = text(&d["major"]["opus"]["summary"]["text"]);
        if found.text.is_empty() {
            found.text = text(&d["desc"]["text"]);
        }
        if found.text.is_empty() {
            found.text = "（该动态没有文字正文）".into();
        }
    }
    found.cover_checked = true;
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(url: &str, raw: Value) -> Item {
        Item {
            ty: "replies".into(),
            url: url.into(),
            raw,
            ..Default::default()
        }
    }
    #[test]
    fn identifiers_round_trip() {
        for (aid, bv) in [
            (170001, "BV17x411w7KC"),
            (455017605, "BV1Q541167Qg"),
            (111298867365120, "BV1L9Uoa9EUx"),
        ] {
            assert_eq!(av_to_bv(aid).as_deref(), Some(bv));
            assert_eq!(bv_to_av(bv), Some(aid.to_string()));
        }
        assert!(bv_to_av("BV1invalid!!").is_none());
    }
    #[test]
    fn canonical_subjects_and_conflicts() {
        let a = resolve(&row(
            "https://www.bilibili.com/video/BV17x411w7KC?p=2#reply4",
            Value::Null,
        ))
        .unwrap();
        assert_eq!(a.key, "video:aid:170001");
        let b = resolve(&row(
            "https://www.bilibili.com/opus/18446744073709551614?comment_on=1",
            json!({"item":{"type":"reply","subject_id":"2"}}),
        ))
        .unwrap();
        assert_eq!(b.key, "dynamic:18446744073709551614");
        assert_eq!(
            resolve(&row(
                "https://t.bilibili.com/18446744073709551614#reply3",
                Value::Null
            ))
            .unwrap()
            .key,
            b.key
        );
        assert!(resolve(&row(
            "https://t.bilibili.com/42",
            json!({"item":{"native_uri":"bilibili://video/42"}})
        ))
        .is_none());
        assert!(resolve(&row("", json!({"item":{"type":"album","subject_id":42}}))).is_none());
        assert_eq!(
            resolve(&row("bilibili://following/detail/42", Value::Null))
                .unwrap()
                .key,
            "dynamic:42"
        );
    }
    #[test]
    fn local_data_and_account_cache_do_not_need_network() {
        let service = Service::default();
        let mut subject = target("video", "170001").unwrap();
        subject.title = "cached title".into();
        subject.cover_checked = true;
        {
            let mut state = service.cache.lock().unwrap();
            state.loaded.insert("12".into());
            state.loaded.insert("13".into());
            state
                .accounts
                .entry("12".into())
                .or_default()
                .insert(subject.key.clone(), subject.clone());
        }
        let found = service.lookup_with("12", target("video", "170001").unwrap(), false, |_| {
            panic!("cached data must not fetch")
        });
        assert_eq!(found.title, "cached title");
        let missing = service.lookup_with("13", target("video", "170001").unwrap(), false, |_| {
            Err(anyhow!("unavailable"))
        });
        assert!(!missing.error.is_empty());
        service.lookup_with("13", target("video", "170001").unwrap(), false, |_| {
            panic!("failed request must not automatically repeat")
        });
        let retried = service.lookup_with("13", target("video", "170001").unwrap(), true, |_| {
            Err(anyhow!("API code -352"))
        });
        assert!(!retried.error.is_empty());
        let blocked = service.lookup_with("13", target("video", "42").unwrap(), false, |_| {
            panic!("risk must suspend other topics")
        });
        assert!(blocked.error.contains("风控"));
        let mut messages = Doc::new(12);
        messages
            .items
            .push(row("https://www.bilibili.com/video/av170001", Value::Null));
        let archive = ArchiveDoc {
            videos: vec![crate::model::Video {
                r#type: "video".into(),
                aid: 170001,
                title: "local archive title".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            local_subjects(&messages, &archive)["video:aid:170001"].title,
            "local archive title"
        );
    }
    #[test]
    fn duplicate_requests_share_one_fetch_and_network_limit_is_two() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let service = Arc::new(Service::default());
        service.cache.lock().unwrap().loaded.insert("12".into());
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let mut threads = vec![];
        for aid in [1, 1, 2, 3, 4] {
            let service = service.clone();
            let active = active.clone();
            let peak = peak.clone();
            let calls = calls.clone();
            threads.push(std::thread::spawn(move || {
                service.lookup_with(
                    "12",
                    target("video", &aid.to_string()).unwrap(),
                    false,
                    |_| {
                        calls.fetch_add(1, Ordering::SeqCst);
                        let n = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(n, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(10));
                        active.fetch_sub(1, Ordering::SeqCst);
                        Err(anyhow!("unavailable"))
                    },
                )
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        assert!(peak.load(Ordering::SeqCst) <= 2);
    }
    #[test]
    fn covers_reuse_attachments_and_old_cache_can_be_checked_once() {
        let mut item = row(
            "https://www.bilibili.com/video/av170001",
            json!({"item":{"type":"video","title":"title","image":"//i0.hdslb.com/cover.jpg"}}),
        );
        item.attachments.push(crate::messages::Attachment {
            id: "a".repeat(32),
            url: "https://i0.hdslb.com/cover.jpg".into(),
            path: "attachments/a.jpg".into(),
            ..Default::default()
        });
        let subject = resolve(&item).unwrap();
        assert!(subject.cover_checked);
        assert_eq!(
            subject.cover_src,
            format!("/api/messages/attachments/{}", "a".repeat(32))
        );
        assert!(!subject.missing());
        let mut old:Subject=serde_json::from_value(json!({"key":"dynamic:42","type":"dynamic","id":"42","title":"","text":"plain text","bvid":"","url":"https://t.bilibili.com/42"})).unwrap();
        assert!(old.missing());
        let mut checked = old.clone();
        checked.cover_checked = true;
        old.merge(&checked);
        assert!(!old.missing());
        assert!(image_url("javascript:alert(1)").is_empty());
    }
}
