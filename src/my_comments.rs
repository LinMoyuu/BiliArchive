//! Complete comments authored by the archive account, keyed by their Bilibili reply ID.
use crate::messages::Item;
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const FORMAT: &str = "bili-my-comment";
const PREVIOUS_FORMAT: &str = "bili-own-comment";
pub const VIEW: &str = "my_comments";

pub fn numeric_id(value: &Value) -> String {
    value
        .as_u64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|id| *id > 0)
        .map(|id| id.to_string())
        .unwrap_or_default()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Location {
    oid: String,
    comment_type: u32,
    root_id: String,
}

pub struct Target {
    pub rpid: String,
    location: Location,
}

pub fn target(item: &Item) -> Option<Target> {
    if !valid_id(&item.my_comment_rpid) {
        return None;
    }
    Some(Target {
        rpid: item.my_comment_rpid.clone(),
        location: location(item)?,
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Detail {
    format: String,
    version: u32,
    pub account_uid: String,
    pub rpid: String,
    location: Location,
    parent_id: String,
    pub text: String,
    timestamp: u64,
    images: Vec<String>,
    #[serde(default)]
    reply_count: Option<u64>,
    #[serde(default)]
    like_count: Option<u64>,
    #[serde(default)]
    counts_sampled: bool,
}

pub fn path(root: &Path, rpid: &str) -> PathBuf {
    root.join("my-comments").join(format!("{rpid}.json"))
}

impl Detail {
    pub fn recognizes_format(format: &str) -> bool {
        matches!(format, FORMAT | PREVIOUS_FORMAT)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            Self::recognizes_format(&self.format)
                && self.version == 1
                && valid_id(&self.account_uid)
                && valid_id(&self.rpid)
                && valid_id(&self.location.oid)
                && self.location.comment_type > 0
                && valid_id(&self.location.root_id),
            "我的评论归档格式无效"
        );
        Ok(())
    }

    fn display_text<'a>(&'a self, excerpt: &'a str) -> &'a str {
        if self.text.trim().is_empty() && self.images.is_empty() {
            excerpt
        } else {
            &self.text
        }
    }
}

pub fn valid_id(id: &str) -> bool {
    id.parse::<u64>()
        .is_ok_and(|n| n > 0 && n.to_string() == id)
}

fn count(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

pub fn read(root: &Path, uid: u64, rpid: &str) -> Option<Detail> {
    if !valid_id(rpid) {
        return None;
    }
    let file = path(root, rpid);
    if !std::fs::symlink_metadata(&file).ok()?.is_file() {
        return None;
    }
    let detail: Detail = serde_json::from_slice(&std::fs::read(file).ok()?).ok()?;
    (detail.account_uid == uid.to_string() && detail.rpid == rpid && detail.validate().is_ok())
        .then_some(detail)
}

pub fn write(root: &Path, detail: &Detail) -> Result<()> {
    detail.validate()?;
    let mut value = serde_json::to_value(detail)?;
    value["format"] = json!(FORMAT);
    crate::secure::write_config(&path(root, &detail.rpid), &value)
}

fn native_comment_url(raw: &Value) -> Option<reqwest::Url> {
    let url = reqwest::Url::parse(raw["native_uri"].as_str()?).ok()?;
    if url.scheme() != "bilibili"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return None;
    }
    Some(url)
}

fn location(item: &Item) -> Option<Location> {
    let rpid = &item.my_comment_rpid;
    let url = native_comment_url(&item.raw["item"])?;
    let parts: Vec<_> = url.path().trim_matches('/').split('/').collect();
    let (comment_type, oid, root) = match (url.host_str()?, parts.as_slice()) {
        ("video", [oid]) => (1, *oid, ""),
        ("comment", ["detail", ty, oid, root]) => (ty.parse::<u32>().ok()?, *oid, *root),
        _ => return None,
    };
    let root = if root.is_empty() {
        url.query_pairs()
            .find(|(name, _)| name == "comment_root_id")
            .map(|(_, value)| value.into_owned())
            .filter(|id| valid_id(id))
            .unwrap_or_else(|| rpid.clone())
    } else {
        root.to_owned()
    };
    (comment_type > 0 && valid_id(oid) && valid_id(&root)).then(|| Location {
        oid: oid.to_owned(),
        comment_type,
        root_id: root,
    })
}

fn find_reply<'a>(value: &'a Value, rpid: &str, uid: &str) -> Option<&'a Value> {
    if numeric_id(&value["rpid"]) == rpid
        && numeric_id(&value["member"]["mid"]) == uid
        && (value["content"]["message"]
            .as_str()
            .is_some_and(|text| !text.trim().is_empty())
            || !crate::bili::pictures_of(value).is_empty())
    {
        return Some(value);
    }
    ["root", "reply"]
        .into_iter()
        .find_map(|key| {
            value
                .get(key)
                .filter(|child| child.is_object())
                .and_then(|child| find_reply(child, rpid, uid))
        })
        .or_else(|| {
            value["replies"]
                .as_array()?
                .iter()
                .find_map(|child| find_reply(child, rpid, uid))
        })
}

pub fn collect(
    root: &Path,
    uid: u64,
    targets: &[Target],
    attempted: &mut BTreeSet<String>,
    mut request: impl FnMut(&str, &str) -> Result<Value>,
    cancelled: impl Fn() -> bool,
) -> Result<usize> {
    let mut unique = BTreeMap::new();
    for target in targets {
        unique
            .entry(target.rpid.as_str())
            .or_insert(&target.location);
    }
    let mut failures = 0;
    let uid_text = uid.to_string();
    for (rpid, location) in unique {
        if cancelled() {
            break;
        }
        if !attempted.insert(rpid.to_owned()) {
            continue;
        }
        let existing = read(root, uid, rpid);
        if existing
            .as_ref()
            .is_some_and(|detail| detail.counts_sampled)
        {
            continue;
        }
        let mut url = format!(
            "https://api.bilibili.com/x/v2/reply/detail?type={}&oid={}&root={}&rpid={}&ps=20&pn=1",
            location.comment_type, location.oid, location.root_id, rpid
        );
        if location.root_id != rpid {
            url.push_str("&seek_rpid=");
            url.push_str(rpid);
        }
        let result = request(rpid, &url);
        if cancelled() {
            break;
        }
        let result = match result {
            Ok(result) => result,
            Err(error)
                if crate::messages::is_persist_failure(&error)
                    || error.to_string().contains("code -101")
                    || error.to_string().contains("账号已变更") =>
            {
                return Err(error)
            }
            Err(_) => {
                failures += 1;
                continue;
            }
        };
        let Some(reply) = find_reply(&result["data"], rpid, &uid_text) else {
            failures += 1;
            continue;
        };
        let images = crate::bili::pictures_of(reply);
        let mut detail = existing.unwrap_or_else(|| Detail {
            format: FORMAT.into(),
            version: 1,
            account_uid: uid_text.clone(),
            rpid: rpid.into(),
            location: location.clone(),
            parent_id: numeric_id(&reply["parent"]),
            text: reply["content"]["message"]
                .as_str()
                .unwrap_or_default()
                .into(),
            timestamp: 0,
            images,
            reply_count: None,
            like_count: None,
            counts_sampled: false,
        });
        detail.reply_count = count(&reply["rcount"])
            .max(count(&reply["count"]))
            .or(detail.reply_count);
        detail.like_count = count(&reply["like"]).or(detail.like_count);
        detail.timestamp = count(&reply["ctime"])
            .filter(|time| *time > 0)
            .unwrap_or(detail.timestamp);
        detail.counts_sampled = true;
        write(root, &detail)
            .with_context(|| format!("保存评论 {rpid} 失败"))
            .context(crate::messages::PersistFailure)?;
    }
    Ok(failures)
}

fn is_target_root(item: &Item) -> bool {
    let raw = &item.raw["item"];
    let target = numeric_id(&raw["target_id"]);
    if target.is_empty() || (!item.my_comment_rpid.is_empty() && item.my_comment_rpid != target) {
        return false;
    }
    let mut roots = Vec::new();
    let root = numeric_id(&raw["root_id"]);
    if !root.is_empty() {
        roots.push(root);
    }
    if let Some(url) = native_comment_url(raw) {
        let path: Vec<_> = url.path().trim_matches('/').split('/').collect();
        if let (Some("comment"), ["detail", ty, oid, root]) = (url.host_str(), path.as_slice()) {
            if [ty, oid, root].into_iter().all(|id| valid_id(id)) {
                roots.push((*root).to_owned());
            }
        }
        if matches!(
            url.host_str(),
            Some("video" | "dynamic" | "opus" | "following" | "article" | "comment")
        ) {
            let mut query_roots = url
                .query_pairs()
                .filter(|(key, _)| key == "comment_root_id");
            if let Some((_, root)) = query_roots.next() {
                if query_roots.next().is_some() || !valid_id(&root) {
                    return false;
                }
                roots.push(root.into_owned());
            }
        }
    }
    !roots.is_empty() && roots.iter().all(|root| *root == target)
}

fn ranked_snippet(item: &Item) -> Option<(u8, &str)> {
    let raw = &item.raw["item"];
    if raw["type"] != "reply" {
        return None;
    }
    let available = |field: &str| raw[field].as_str().filter(|text| !text.trim().is_empty());
    available("target_reply_content")
        .map(|text| (3, text))
        .or_else(|| match item.ty.as_str() {
            "replies" if is_target_root(item) => {
                available("root_reply_content").map(|text| (2, text))
            }
            "likes" => available("title").map(|text| (1, text)),
            _ => None,
        })
}

pub fn snippet(item: &Item) -> &str {
    ranked_snippet(item)
        .map(|(_, text)| text)
        .unwrap_or_default()
}

#[derive(Clone, Copy, Default)]
enum SortField {
    #[default]
    Interaction,
    Replies,
    Likes,
    Published,
}

#[derive(Clone, Copy, Default)]
pub struct Order {
    field: SortField,
    ascending: bool,
}

impl Order {
    pub fn parse(value: &str) -> Option<Self> {
        let (field, ascending) = match value {
            "new" => (SortField::Interaction, false),
            "old" => (SortField::Interaction, true),
            "replies_desc" => (SortField::Replies, false),
            "replies_asc" => (SortField::Replies, true),
            "likes_desc" => (SortField::Likes, false),
            "likes_asc" => (SortField::Likes, true),
            "published_desc" => (SortField::Published, false),
            "published_asc" => (SortField::Published, true),
            _ => return None,
        };
        Some(Self { field, ascending })
    }

    fn compare(self, left: Option<u64>, right: Option<u64>) -> Ordering {
        match (left, right) {
            (Some(left), Some(right)) if self.ascending => left.cmp(&right),
            (Some(left), Some(right)) => right.cmp(&left),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
    }
}

struct CommentGroup<'a> {
    item: &'a Item,
    excerpt: (u8, &'a str),
    sources: BTreeSet<&'a str>,
    replies: BTreeSet<&'a str>,
    notified_likes: Option<u64>,
}

struct RankedComment<'a> {
    rpid: &'a str,
    group: CommentGroup<'a>,
    detail: Option<Detail>,
    defer_detail: bool,
    sort_key: Option<u64>,
}

fn ranked_comments<'a>(
    items: impl IntoIterator<Item = &'a Item>,
    q: &str,
    order: Order,
    detail: &mut impl FnMut(&str) -> Option<Detail>,
) -> Vec<RankedComment<'a>> {
    let mut groups = BTreeMap::new();
    for item in items.into_iter().filter(|item| {
        valid_id(&item.my_comment_rpid) && matches!(item.ty.as_str(), "replies" | "likes")
    }) {
        let group = groups
            .entry(item.my_comment_rpid.as_str())
            .or_insert_with(|| CommentGroup {
                item,
                excerpt: (0, ""),
                sources: BTreeSet::new(),
                replies: BTreeSet::new(),
                notified_likes: None,
            });
        if (item.timestamp, &item.id) > (group.item.timestamp, &group.item.id) {
            group.item = item;
        }
        if let Some((rank, text)) = ranked_snippet(item) {
            if (rank, text.len()) > (group.excerpt.0, group.excerpt.1.len()) {
                group.excerpt = (rank, text);
            }
        }
        group.sources.insert(item.ty.as_str());
        if item.ty == "replies" {
            group.replies.insert(if item.reply_rpid.is_empty() {
                &item.id
            } else {
                &item.reply_rpid
            });
        } else {
            let reported = count(&item.raw["counts"])
                .max(count(&item.raw["count"]))
                .max((item.like_count > 0).then_some(item.like_count));
            group.notified_likes = group.notified_likes.max(reported);
        }
    }
    let needle = q.to_lowercase();
    let read_for_sort = !matches!(order.field, SortField::Interaction);
    let mut matches = Vec::new();
    for (rpid, group) in groups {
        let interaction = (group.item.timestamp > 0).then_some(group.item.timestamp);
        let defer_detail = !read_for_sort && needle.is_empty() && interaction.is_some();
        let full = (!defer_detail).then(|| detail(rpid)).flatten();
        let (_, excerpt) = group.excerpt;
        let text = full.as_ref().map_or(excerpt, |d| d.display_text(excerpt));
        let raw = &group.item.raw["item"];
        if !needle.is_empty()
            && ![text, rpid, raw["title"].as_str().unwrap_or_default()]
                .iter()
                .any(|part| part.to_lowercase().contains(&needle))
        {
            continue;
        }
        let published = full.as_ref().map(|d| d.timestamp).filter(|&time| time > 0);
        let key = match order.field {
            SortField::Interaction => interaction.or(published),
            SortField::Replies => Some(
                full.as_ref()
                    .and_then(|d| d.reply_count)
                    .unwrap_or(group.replies.len() as u64),
            ),
            SortField::Likes => full
                .as_ref()
                .and_then(|d| d.like_count)
                .or(group.notified_likes),
            SortField::Published => published.or(interaction),
        };
        matches.push(RankedComment { rpid, group, detail: full, defer_detail, sort_key: key });
    }
    matches.sort_unstable_by(|left, right| {
        order
            .compare(left.sort_key, right.sort_key)
            .then_with(|| right.group.item.timestamp.cmp(&left.group.item.timestamp))
            .then_with(|| (right.rpid.len(), right.rpid).cmp(&(left.rpid.len(), left.rpid)))
    });
    matches
}

impl RankedComment<'_> {
    fn subject(&self) -> Option<crate::message_subjects::Subject> {
        crate::message_subjects::resolve(self.group.item)
    }

    fn row(&self, detail: &mut impl FnMut(&str) -> Option<Detail>) -> Value {
        let loaded = self.defer_detail.then(|| detail(self.rpid)).flatten();
        let full = self.detail.as_ref().or(loaded.as_ref());
        let CommentGroup { item, excerpt: (_, excerpt), sources, replies, notified_likes } = &self.group;
        let text = full.map_or(*excerpt, |d| d.display_text(excerpt));
        let raw = &item.raw["item"];
        let reply_total = full.and_then(|d| d.reply_count);
        let site_likes = full.and_then(|d| d.like_count);
        json!({"id":self.rpid,"type":VIEW,"timestamp":item.timestamp,
            "text":text,"sources":sources,"reply_count":replies.len(),"my_comment":full,
            "reply_total":reply_total,"like_count":site_likes.or(*notified_likes),
            "like_count_from_notification":site_likes.is_none() && notified_likes.is_some(),
            "subject":self.subject(),"raw":{"item":{"native_uri":raw["native_uri"],"uri":raw["uri"],"type":"reply"}},"url":item.url})
    }
}

pub fn page<'a>(
    items: impl IntoIterator<Item = &'a Item>,
    q: &str,
    order: Order,
    offset: usize,
    limit: usize,
    detail: impl FnMut(&str) -> Option<Detail>,
) -> (usize, Vec<Value>) {
    member_page(items, q, order, None, offset, limit, detail)
}

pub fn member_page<'a>(
    items: impl IntoIterator<Item = &'a Item>,
    q: &str,
    order: Order,
    subject_key: Option<&str>,
    offset: usize,
    limit: usize,
    mut detail: impl FnMut(&str) -> Option<Detail>,
) -> (usize, Vec<Value>) {
    let mut matches = ranked_comments(items, q, order, &mut detail);
    if let Some(key) = subject_key {
        matches.retain(|row| row.subject().is_some_and(|subject| subject.key == key));
    }
    let rows = matches.iter().skip(offset).take(limit)
        .map(|row| row.row(&mut detail)).collect();
    (matches.len(), rows)
}

pub fn subject_page<'a>(
    items: impl IntoIterator<Item = &'a Item>,
    q: &str,
    order: Order,
    offset: usize,
    limit: usize,
    mut detail: impl FnMut(&str) -> Option<Detail>,
) -> (usize, usize, Vec<Value>) {
    struct Topic {
        first: usize,
        count: usize,
        subject: Option<crate::message_subjects::Subject>,
    }
    let matches = ranked_comments(items, q, order, &mut detail);
    let mut topics: Vec<Topic> = Vec::new();
    let mut indices = BTreeMap::new();
    for (index, row) in matches.iter().enumerate() {
        let subject = row.subject();
        if let Some(subject) = &subject {
            if let Some(&topic) = indices.get(&subject.key) {
                let topic: &mut Topic = &mut topics[topic];
                topic.subject.as_mut().unwrap().merge(subject);
                topic.count += 1;
                continue;
            }
            indices.insert(subject.key.clone(), topics.len());
        }
        topics.push(Topic { first: index, count: 1, subject });
    }
    let groups = topics.iter().skip(offset).take(limit).map(|topic| {
        if let Some(subject) = &topic.subject {
            json!({"key":subject.key,"subject":subject,"count":topic.count,"standalone":false})
        } else {
            let row = &matches[topic.first];
            json!({"key":format!("my-comment:{}", row.rpid),"standalone":true,
                "count":1,"item":row.row(&mut detail)})
        }
    }).collect();
    (topics.len(), matches.len(), groups)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_only_comment_round_trips_in_a_portable_zip() {
        let work = tempfile::tempdir().unwrap();
        let files = work.path().join("original");
        let root = files.join("archive-v1/12/messages");
        let item = Item {
            id: "1".into(),
            ty: "likes".into(),
            my_comment_rpid: "123".into(),
            raw: json!({"item":{"native_uri":"bilibili://video/42?comment_root_id=123"}}),
            ..Item::default()
        };
        assert_eq!(collect(&root, 12, &[target(&item).unwrap()], &mut BTreeSet::new(), |_, url| {
            assert!(url.contains("root=123&rpid=123"));
            assert!(!url.contains("seek_rpid"));
            Ok(json!({"data":{"root":{"rpid":"123","member":{"mid":"12"},"ctime":1700000000,"content":{"pictures":[{"img_src":"//i0.hdslb.com/photo/a"},{"img_src":"","img_url":"http://i0.hdslb.com/photo/b"}]}}}}))
        }, || false).unwrap(), 0);
        let zip = work.path().join("bundle.zip");
        crate::portable_zip::create(
            &zip,
            &[(
                "archive-v1/12/messages/my-comments/123.json".into(),
                path(&root, "123"),
            )],
        )
        .unwrap();
        let restored = work.path().join("restored");
        crate::portable_zip::extract_with_workers(&zip, &restored, 1).unwrap();
        let detail = read(&restored.join("archive-v1/12/messages"), 12, "123").unwrap();
        assert!(detail.text.is_empty());
        assert_eq!(detail.timestamp, 1700000000);
        assert_eq!(
            detail.images,
            [
                "https://i0.hdslb.com/photo/a",
                "https://i0.hdslb.com/photo/b"
            ]
        );
        let restored_root = restored.join("archive-v1/12/messages");
        let restored_file = path(&restored_root, "123");
        let mut previous = serde_json::to_value(&detail).unwrap();
        previous["format"] = json!(PREVIOUS_FORMAT);
        for key in ["counts_sampled", "reply_count", "like_count"] {
            previous.as_object_mut().unwrap().remove(key);
        }
        crate::secure::write_config(&restored_file, &previous).unwrap();
        let target = Target {
            rpid: detail.rpid,
            location: detail.location,
        };
        collect(
            &restored_root,
            12,
            &[target],
            &mut BTreeSet::new(),
            |_, _| {
                Ok(
                    json!({"data":{"root":{"rpid":123,"member":{"mid":12},"like":0,"rcount":"7",
                "content":{"message":"本次只补统计"}}}}),
                )
            },
            || false,
        )
        .unwrap();
        let rewritten: Value =
            serde_json::from_slice(&std::fs::read(restored_file).unwrap()).unwrap();
        assert_eq!(rewritten["format"], FORMAT);
        assert_eq!(rewritten["like_count"], 0);
        assert_eq!(rewritten["reply_count"], 7);
        assert_eq!(rewritten["timestamp"], 1700000000);
        assert_eq!(rewritten["images"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn sorts_filtered_comments_before_paging_and_keeps_unknown_values_last() {
        let notice = |rpid: &str, id: &str, timestamp, likes: Option<u64>, text: &str| Item {
            id: id.into(),
            ty: "likes".into(),
            my_comment_rpid: rpid.into(),
            timestamp,
            raw: json!({"counts":likes,"item":{"type":"reply","title":text}}),
            ..Item::default()
        };
        let items = [
            notice("101", "1", 30, Some(99), "ignored"),
            notice("102", "2", 20, None, "other"),
            notice("103", "3", 40, Some(9), "needle"),
            notice("103", "4", 10, Some(8), "needle"),
            notice("104", "5", 50, None, "unknown"),
            notice("105", "6", 50, None, "unknown"),
            Item {
                id: "7".into(),
                ty: "replies".into(),
                my_comment_rpid: "103".into(),
                ..Item::default()
            },
        ];
        let details = |id: &str| {
            let (timestamp, reply_count, like_count, text) = match id {
                "101" => (200, 0, 0, "needle"),
                "102" => (100, 5, 6, "other"),
                _ => return None,
            };
            Some(Detail {
                format: FORMAT.into(),
                version: 1,
                account_uid: "12".into(),
                rpid: id.into(),
                location: Location {
                    oid: "42".into(),
                    comment_type: 1,
                    root_id: id.into(),
                },
                parent_id: String::new(),
                text: text.into(),
                timestamp,
                images: Vec::new(),
                reply_count: Some(reply_count),
                like_count: Some(like_count),
                counts_sampled: true,
            })
        };
        for (key, expected) in [
            ("replies_desc", ["102", "103", "105", "104", "101"]),
            ("replies_asc", ["105", "104", "101", "103", "102"]),
            ("likes_desc", ["103", "102", "101", "105", "104"]),
            ("likes_asc", ["101", "102", "103", "105", "104"]),
            ("published_desc", ["101", "102", "105", "104", "103"]),
            ("published_asc", ["103", "105", "104", "102", "101"]),
        ] {
            let (total, rows) = page(&items, "", Order::parse(key).unwrap(), 0, 20, details);
            assert_eq!(total, 5, "{key}");
            assert_eq!(
                rows.iter()
                    .map(|row| row["id"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                expected,
                "{key}"
            );
        }
        let order = Order::parse("likes_desc").unwrap();
        let (total, first) = page(&items, "needle", order, 0, 1, details);
        let (_, second) = page(&items, "needle", order, 1, 1, details);
        assert_eq!(total, 2);
        assert_eq!(first[0]["like_count"], 9);
        assert_eq!(first[0]["like_count_from_notification"], true);
        assert_eq!(first[0]["reply_count"], 1);
        assert!(first[0]["reply_total"].is_null());
        assert_eq!(second[0]["reply_total"], 0);
        assert_eq!(second[0]["like_count"], 0);

        let time_items = [
            notice("101", "1", 10, None, ""), // published 200, interaction 10
            notice("102", "2", 0, None, ""),  // published 100, no interaction
            notice("103", "3", 100, None, ""), // interaction ties with 102's fallback
            notice("104", "4", 50, None, ""),
            notice("105", "5", 0, None, ""),  // both missing
        ];
        for (key, expected) in [
            ("published_desc", ["101", "103", "102", "104", "105"]),
            ("published_asc", ["104", "103", "102", "101", "105"]),
            ("new", ["103", "102", "104", "101", "105"]),
            ("old", ["101", "104", "103", "102", "105"]),
        ] {
            let order = Order::parse(key).unwrap();
            let (total, first) = page(&time_items, "", order, 0, 2, details);
            let (_, second) = page(&time_items, "", order, 2, 3, details);
            assert_eq!(total, 5);
            assert_eq!(
                first.iter().chain(&second).map(|row| row["id"].as_str().unwrap()).collect::<Vec<_>>(),
                expected, "{key}"
            );
        }

        let mut reads = Vec::new();
        let (_, rows) = page(&time_items, "", Order::parse("new").unwrap(), 1, 1, |id| {
            reads.push(id.to_owned());
            details(id)
        });
        assert_eq!(reads, ["102", "105"]);
        assert_eq!(rows[0]["timestamp"], 0);
        assert_eq!(rows[0]["my_comment"]["timestamp"], 100);
    }

    #[test]
    fn own_subject_groups_deduplicate_before_paging_and_preserve_member_order() {
        let notice = |rpid: &str, time: u64, url: &str, text: &str| Item {
            id: time.to_string(),
            ty: "replies".into(),
            timestamp: time,
            my_comment_rpid: rpid.into(),
            url: url.into(),
            raw: json!({"item":{"type":"reply","title":"same topic title","target_reply_content":text}}),
            ..Item::default()
        };
        let items = [
            notice("101", 40u64, "https://www.bilibili.com/video/BV17x411w7KC", "needle"),
            notice("101", 35u64, "https://www.bilibili.com/video/av170001", "needle"),
            notice("102", 10u64, "https://www.bilibili.com/video/av170001", "needle"),
            notice("103", 30u64, "https://t.bilibili.com/170001", "needle"),
            notice("104", 20u64, "", "needle"),
            notice("105", 50u64, "https://t.bilibili.com/170001", "other"),
        ];
        let order = Order::parse("new").unwrap();
        let (total, comments, first) = subject_page(&items, "needle", order, 0, 1, |_| None);
        assert_eq!((total, comments), (3, 4));
        assert_eq!(first[0]["count"], 2);
        let key = first[0]["key"].as_str().unwrap();
        let (_, _, later) = subject_page(&items, "needle", order, 1, 2, |_| None);
        assert_eq!(later[0]["subject"]["type"], "dynamic");
        assert_eq!(later[1]["item"]["id"], "104");
        let (total, members) = member_page(&items, "needle", order, Some(key), 1, 1, |_| None);
        assert_eq!(total, 2);
        assert_eq!(members[0]["id"], "102");
        let (_, members) = member_page(&items, "needle", Order::parse("old").unwrap(), Some(key), 0, 1, |_| None);
        assert_eq!(members[0]["id"], "102");
    }

    #[test]
    fn only_confirmed_roots_can_supply_an_excerpt() {
        let mut root = Item {
            ty: "replies".into(),
            my_comment_rpid: "123".into(),
            raw: json!({"item":{"type":"reply","target_id":"123","root_id":0,
                "native_uri":"bilibili://video/42?comment_root_id=123&comment_secondary_id=999",
                "root_reply_content":"这是同一个目标根评论的较长摘要"}}),
            ..Item::default()
        };
        let confirmed = root.clone();
        assert_eq!(snippet(&root), "这是同一个目标根评论的较长摘要");
        root.raw["item"]["root_id"] = json!("222");
        assert!(snippet(&root).is_empty());
        root.raw["item"]["root_id"] = json!(0);
        root.raw["item"]["native_uri"] = json!("bilibili://video/42?comment_root_id=456");
        assert!(snippet(&root).is_empty());
        root.raw["item"]["native_uri"] = json!("bilibili://video/42");
        assert!(snippet(&root).is_empty());
        root.raw["item"]["target_reply_content"] = json!("直引");
        let (_, rows) = page([&confirmed, &root], "", Order::default(), 0, 50, |_| None);
        assert_eq!(rows[0]["text"], "直引");
        assert_eq!(
            crate::messages::item_summary(&confirmed)["comment_excerpt"],
            snippet(&confirmed)
        );
    }

    #[test]
    fn merges_notification_sources_and_keeps_only_the_authenticated_reply() {
        let root = tempfile::tempdir().unwrap();
        let rpid = "18446744073709551614";
        let reply = Item {
            id: "1".into(),
            ty: "replies".into(),
            reply_rpid: "992".into(),
            my_comment_rpid: rpid.into(),
            timestamp: 10,
            raw: json!({"item":{"type":"reply","native_uri":"bilibili://video/42?comment_root_id=123&comment_secondary_id=992","target_reply_content":"摘要"}}),
            ..Item::default()
        };
        let like = Item {
            id: "2".into(),
            ty: "likes".into(),
            timestamp: 11,
            ..reply.clone()
        };
        let items = [reply, like];
        let targets: Vec<_> = items.iter().filter_map(target).collect();
        let mut calls = 0;
        let fetch = |id: &str, url: &str| {
            calls += 1;
            assert_eq!(id, rpid);
            assert!(url.contains("root=123"));
            Ok(json!({"data":{"root":{"rpid":"123","member":{"mid":"12"},
                "content":{"message":"other root"},"replies":[
                {"rpid":rpid,"member":{"mid":"4"},"content":{"message":"wrong author"}},
                {"rpid":rpid,"member":{"mid":"12"},"content":{"message":"完整正文"}}
            ]}}}))
        };
        assert_eq!(
            collect(
                root.path(),
                12,
                &targets,
                &mut BTreeSet::new(),
                fetch,
                || false
            )
            .unwrap(),
            0
        );
        assert_eq!(calls, 1);
        assert!(read(root.path(), 4, rpid).is_none());
        assert_eq!(
            collect(
                root.path(),
                12,
                &targets,
                &mut BTreeSet::new(),
                |_, _| panic!("already stored"),
                || false
            )
            .unwrap(),
            0
        );
        let (total, rows) = page(&items, "完整", Order::default(), 0, 50, |id| {
            read(root.path(), 12, id)
        });
        assert_eq!(total, 1);
        assert_eq!(rows[0]["text"], "完整正文");
        assert_eq!(rows[0]["sources"], json!(["likes", "replies"]));
        assert_eq!(rows[0]["reply_count"], 1);
        let (_, rows) = page(&items, "", Order::default(), 0, 50, |_| None);
        assert_eq!(rows[0]["text"], "摘要");
    }
}
