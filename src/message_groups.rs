//! Read-only grouping derived from existing message archives; IDs never pass through floats.
use crate::messages::{Doc, Item};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
fn positive(value: &str) -> Option<String> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let v = value.trim_start_matches('0');
    (!v.is_empty()).then(|| v.to_owned())
}
pub struct Group<'a> {
    pub key: String,
    pub label: String,
    pub identifier: String,
    pub url: String,
    pub standalone: bool,
    pub items: Vec<&'a Item>,
    first_index: usize,
    name_time: u64,
}
impl Group<'_> {
    fn first_time(&self) -> u64 {
        self.items
            .iter()
            .map(|i| i.timestamp)
            .filter(|n| *n > 0)
            .min()
            .unwrap_or(0)
    }
    fn last_time(&self) -> u64 {
        self.items.iter().map(|i| i.timestamp).max().unwrap_or(0)
    }
    pub fn summary(&self) -> Value {
        let mut value = json!({"key":self.key,"label":self.label,"identifier":self.identifier,"url":self.url,"standalone":self.standalone,"count":self.items.len(),"first_time":self.first_time(),"last_time":self.last_time()});
        if self.key.starts_with("user:") {
            let avatar = self
                .items
                .iter()
                .filter_map(|item| {
                    let raw = &item.raw;
                    let user = if raw["user"].is_object() {
                        &raw["user"]
                    } else {
                        &raw["users"][0]
                    };
                    let image = [&user["avatar"], &user["face"]]
                        .into_iter()
                        .filter_map(Value::as_str)
                        .map(crate::message_subjects::image_url)
                        .find(|s| !s.is_empty())?;
                    Some((item.timestamp, image))
                })
                .max_by_key(|(time, _)| *time)
                .map(|(_, image)| image)
                .unwrap_or_default();
            value["avatar_url"] = json!(avatar);
        }
        if self.standalone {
            value["item"] = crate::message_subjects::summary(self.items[0]);
        }
        if !self.standalone && !self.key.starts_with("user:") {
            let mut subject = crate::message_subjects::resolve(self.items[0]);
            if let Some(ref mut subject) = subject {
                for item in &self.items {
                    if let Some(other) = crate::message_subjects::resolve(item) {
                        subject.merge(&other);
                    }
                }
            }
            value["subject"] = json!(subject);
        }
        value
    }
}
/// Rank the complete result before API pagination; member chronology stays untouched.
pub fn sort_groups(groups: &mut [Group<'_>], sort: &str) -> Result<()> {
    if sort == "time" {
        return Ok(());
    }
    if !matches!(sort, "count_desc" | "count_asc") {
        bail!("unsupported group sort");
    }
    groups.sort_by(|a, b| {
        let count = if sort == "count_asc" {
            a.items.len().cmp(&b.items.len())
        } else {
            b.items.len().cmp(&a.items.len())
        };
        count
            .then_with(|| b.last_time().cmp(&a.last_time()))
            .then_with(|| a.key.cmp(&b.key))
    });
    Ok(())
}
pub fn query<'a>(
    doc: &'a Doc,
    ty: &str,
    mode: &str,
    q: &str,
    ascending: bool,
) -> Result<Vec<Group<'a>>> {
    if !matches!(ty, "replies" | "mentions") || !matches!(mode, "user" | "video") {
        bail!("unsupported message grouping");
    }
    let subjects = crate::message_subjects::index(doc);
    let mut groups: BTreeMap<String, Group<'a>> = BTreeMap::new();
    for (index, item) in doc.query(ty, "", q, ascending).into_iter().enumerate() {
        let descriptor = if mode == "user" {
            positive(&item.sender_uid).map(|uid| {
                (
                    format!("user:{uid}"),
                    if item.sender_name.is_empty() {
                        format!("用户 {uid}")
                    } else {
                        item.sender_name.clone()
                    },
                    format!("UID {uid}"),
                    String::new(),
                )
            })
        } else {
            crate::message_subjects::resolve(item).map(|target| {
                let subject = &subjects[&target.key];
                let identifier = if subject.ty == "video" {
                    subject.bvid.clone()
                } else {
                    format!(
                        "{} {}",
                        if subject.ty == "dynamic" {
                            "动态"
                        } else {
                            "专栏"
                        },
                        subject.id
                    )
                };
                let label = if !subject.title.is_empty() {
                    subject.title.clone()
                } else if !subject.text.is_empty() {
                    subject.text.chars().take(80).collect()
                } else {
                    identifier.clone()
                };
                (subject.key.clone(), label, identifier, subject.url.clone())
            })
        };
        let standalone = descriptor.is_none();
        let (key, label, identifier, url) = descriptor.unwrap_or_else(|| {
            (
                format!("item:{ty}:{}:{index}", item.id),
                String::new(),
                String::new(),
                String::new(),
            )
        });
        let group = groups.entry(key.clone()).or_insert_with(|| Group {
            key,
            label,
            identifier,
            url,
            standalone,
            items: vec![],
            first_index: index,
            name_time: 0,
        });
        if mode == "user"
            && !standalone
            && !item.sender_name.is_empty()
            && (group.name_time == 0 || item.timestamp > group.name_time)
        {
            group.label = item.sender_name.clone();
            group.name_time = item.timestamp;
        }
        group.items.push(item);
    }
    let mut groups: Vec<_> = groups.into_values().collect();
    groups.sort_by(|a, b| {
        let a_time = if ascending {
            a.first_time()
        } else {
            a.last_time()
        };
        let b_time = if ascending {
            b.first_time()
        } else {
            b.last_time()
        };
        match (a_time, b_time) {
            (0, 0) => a.first_index.cmp(&b.first_index),
            (0, _) => std::cmp::Ordering::Greater,
            (_, 0) => std::cmp::Ordering::Less,
            _ => {
                let cmp = if ascending {
                    a_time.cmp(&b_time)
                } else {
                    b_time.cmp(&a_time)
                };
                cmp.then_with(|| a.first_index.cmp(&b.first_index))
            }
        }
    });
    Ok(groups)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn item(key: &str, uid: &str, time: u64, url: &str, raw: Value) -> Item {
        Item {
            id: key.into(),
            ty: "replies".into(),
            sender_uid: uid.into(),
            sender_name: "same nickname".into(),
            timestamp: time,
            url: url.into(),
            raw,
            ..Default::default()
        }
    }
    #[test]
    fn count_ranking_is_global_stable_and_keeps_member_chronology() {
        for ty in ["replies", "mentions"] {
            for mode in ["user", "video"] {
                let mut doc = Doc::new(1);
                for (uid, count, time) in [("1", 2, 30), ("2", 2, 30), ("3", 2, 0), ("4", 1, 999)] {
                    for n in 0..count {
                        let mut row = item(
                            &format!("{uid}-{n}"),
                            uid,
                            time,
                            &format!("https://www.bilibili.com/video/av{uid}"),
                            Value::Null,
                        );
                        row.ty = ty.into();
                        row.text = if uid == "1" || uid == "2" && n == 0 {
                            "needle".into()
                        } else {
                            "other".into()
                        };
                        doc.items.push(row);
                    }
                }
                let mut groups = query(&doc, ty, mode, "", true).unwrap();
                let members: BTreeMap<_, _> = groups
                    .iter()
                    .map(|g| {
                        (
                            g.key.clone(),
                            g.items.iter().map(|i| i.id.clone()).collect::<Vec<_>>(),
                        )
                    })
                    .collect();
                sort_groups(&mut groups, "count_desc").unwrap();
                assert!(groups[0].key.ends_with(":1"));
                assert!(groups[1].key.ends_with(":2"));
                assert!(groups[2].key.ends_with(":3"));
                assert!(groups[3].key.ends_with(":4"));
                for group in &groups {
                    assert_eq!(
                        &members[&group.key],
                        &group.items.iter().map(|i| i.id.clone()).collect::<Vec<_>>()
                    );
                }
                sort_groups(&mut groups, "count_asc").unwrap();
                assert!(groups[0].key.ends_with(":4"));
                assert!(groups[1].key.ends_with(":1"));
                let mut searched = query(&doc, ty, mode, "needle", false).unwrap();
                sort_groups(&mut searched, "count_desc").unwrap();
                assert_eq!(searched.len(), 2);
                assert_eq!(searched[0].items.len(), 2);
                assert_eq!(searched[1].items.len(), 1);
                assert!(sort_groups(&mut groups, "invalid").is_err());
            }
        }
    }
    #[test]
    fn user_groups_cross_pages_keep_ids_and_do_not_merge_unknown_users() {
        let mut doc = Doc::new(1);
        for n in 0..120 {
            doc.items.push(item(
                &n.to_string(),
                "18446744073709551614",
                n,
                "",
                Value::Null,
            ));
        }
        doc.items.push(item("other", "2", 0, "", Value::Null));
        doc.items.push(item("unknown1", "", 0, "", Value::Null));
        doc.items.push(item("unknown2", "0", 0, "", Value::Null));
        let groups = query(&doc, "replies", "user", "", false).unwrap();
        assert_eq!(groups.len(), 4);
        assert_eq!(groups[0].items.len(), 120);
        assert_eq!(groups[0].key, "user:18446744073709551614");
        assert!(groups[2..].iter().all(|g| g.standalone));
        assert!(query(&doc, "likes", "user", "", false).is_err());
    }
    #[test]
    fn video_links_unify_without_merging_dynamics_or_ambiguous_targets() {
        let mut doc = Doc::new(1);
        doc.items = vec![
            item(
                "a",
                "1",
                3,
                "https://www.bilibili.com/video/BV1xx411c7UL?p=2",
                json!({"item":{"native_uri":"bilibili://video/42?comment_root_id=9","subject_id":42,"type":"video","title":"Video title"}}),
            ),
            item(
                "b",
                "1",
                1,
                "https://www.bilibili.com/video/av42#reply9",
                Value::Null,
            ),
            item(
                "c",
                "1",
                2,
                "https://www.bilibili.com/video/BV1xx411c7UL",
                Value::Null,
            ),
            item(
                "dynamic",
                "1",
                4,
                "https://t.bilibili.com/42",
                json!({"item":{"subject_id":42,"type":"dynamic"}}),
            ),
            item(
                "conflict",
                "1",
                0,
                "https://www.bilibili.com/video/av42",
                json!({"item":{"native_uri":"bilibili://video/99"}}),
            ),
        ];
        let groups = query(&doc, "replies", "video", "", false).unwrap();
        assert_eq!(groups.len(), 3);
        let video = groups.iter().find(|g| g.key == "video:aid:42").unwrap();
        assert_eq!(video.items.len(), 3);
        assert_eq!(video.label, "Video title");
        assert_eq!(video.key, "video:aid:42");
        assert_eq!(
            query(&doc, "replies", "video", "", true).unwrap()[0].key,
            "video:aid:42"
        );
        assert_eq!(
            query(&doc, "replies", "user", "Video title", false)
                .unwrap()
                .len(),
            0
        ); // Search semantics stay unchanged.
    }
    #[test]
    fn subjects_merge_direct_comments_and_nested_replies_across_pages() {
        for ty in ["replies", "mentions"] {
            let mut doc = Doc::new(1);
            for n in 0..120 {
                let url = if n % 2 == 0 {
                    "https://t.bilibili.com/987#reply1"
                } else {
                    "https://www.bilibili.com/opus/987?comment_root_id=2"
                };
                let mut row = item(
                    &n.to_string(),
                    "1",
                    n,
                    url,
                    json!({"item":{"type":if n%2==0{"dynamic"}else{"reply"},"subject_id":42,"title":if n%2==0{"dynamic topic"}else{"original comment"}}}),
                );
                row.ty = ty.into();
                doc.items.push(row);
            }
            let mut video = item(
                "video",
                "1",
                1,
                "https://www.bilibili.com/video/av987",
                Value::Null,
            );
            video.ty = ty.into();
            doc.items.push(video);
            let groups = query(&doc, ty, "video", "", false).unwrap();
            assert_eq!(groups.len(), 2);
            let topic = groups.iter().find(|g| g.key == "dynamic:987").unwrap();
            assert_eq!(topic.items.len(), 120);
            assert_eq!(topic.summary()["subject"]["text"], "dynamic topic");
            assert_eq!(topic.items.iter().skip(100).count(), 20);
            assert!(groups.iter().any(|g| g.key == "video:aid:987"));
        }
    }
    #[test]
    fn user_avatar_uses_latest_valid_image_without_changing_group_members() {
        let mut doc = Doc::new(1);
        doc.items = vec![
            item(
                "old",
                "2",
                1,
                "",
                json!({"user":{"avatar":"https://example.test/old.jpg"}}),
            ),
            item(
                "new",
                "2",
                2,
                "",
                json!({"user":{"face":"https://example.test/new.jpg"}}),
            ),
            item(
                "invalid",
                "2",
                3,
                "",
                json!({"user":{"avatar":"javascript:bad"}}),
            ),
        ];
        let groups = query(&doc, "replies", "user", "", false).unwrap();
        assert_eq!(
            groups[0].summary()["avatar_url"],
            "https://example.test/new.jpg"
        );
        assert_eq!(groups[0].items.len(), 3);
    }
}
