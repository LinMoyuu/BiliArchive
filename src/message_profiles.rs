//! Account-scoped, on-demand presentation metadata for archived private senders.
use crate::{
    message_subjects::{image_url, Credentials},
    messages::Doc,
};
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

pub type Pacing = Arc<dyn Fn() -> (u64, u64) + Send + Sync>;
#[derive(Clone, Default, Serialize, Deserialize, Debug)]
pub struct Profile {
    pub uid: String,
    pub name: String,
    pub avatar_url: String,
    pub error: String,
}
fn id(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|b| b.is_ascii_digit())
        && !value.starts_with('0')
        && value.parse::<u64>().is_ok_and(|n| n > 0)
}
fn raw_id(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|n| n.to_string()))
        .unwrap_or_default()
}
fn avatar_url(raw: &str) -> String {
    let url = image_url(raw);
    if url
        .split(['?', '#'])
        .next()
        .is_some_and(|s| s.ends_with("/member/noface.jpg"))
    {
        String::new()
    } else {
        url
    }
}
fn avatar(raw: &Value) -> String {
    [
        &raw["account_info"]["pic"],
        &raw["account_info"]["face"],
        &raw["user"]["avatar"],
        &raw["user"]["face"],
        &raw["sender_face"],
        &raw["face"],
        &raw["avatar"],
    ]
    .into_iter()
    .filter_map(Value::as_str)
    .map(avatar_url)
    .find(|s| !s.is_empty())
    .unwrap_or_default()
}
pub fn local_profiles(doc: &Doc) -> BTreeMap<String, Profile> {
    let mut profiles = BTreeMap::<String, Profile>::new();
    for session in doc
        .sessions
        .iter()
        .filter(|s| s.session_type == 1 && id(&s.talker_id))
    {
        let entry = profiles
            .entry(session.talker_id.clone())
            .or_insert_with(|| Profile {
                uid: session.talker_id.clone(),
                ..Default::default()
            });
        if !session.name.is_empty() {
            entry.name = session.name.clone();
        }
        let photo = avatar(&session.raw);
        if !photo.is_empty() {
            entry.avatar_url = photo;
        }
    }
    for item in doc
        .items
        .iter()
        .filter(|i| i.ty == "private" && id(&i.sender_uid))
    {
        let entry = profiles
            .entry(item.sender_uid.clone())
            .or_insert_with(|| Profile {
                uid: item.sender_uid.clone(),
                ..Default::default()
            });
        if entry.name.is_empty() {
            entry.name = item.sender_name.clone();
        }
        // User metadata must describe this sender, never another participant.
        let user_id = raw_id(&item.raw["user"]["mid"]);
        if entry.avatar_url.is_empty() && (user_id.is_empty() || user_id == item.sender_uid) {
            entry.avatar_url = avatar(&item.raw);
        }
    }
    // Only enrich users already associated with a private session/message.
    // Other categories are donors, not permission to query arbitrary people.
    for item in &doc.items {
        let raw = &item.raw;
        if raw["user"].is_object() {
            let user = &raw["user"];
            let key = user_id(user).unwrap_or_else(|| item.sender_uid.clone());
            reuse_user(&mut profiles, &key, user);
        }
        for users in [&raw["users"], &raw["item"]["at_details"]] {
            if let Some(users) = users.as_array() {
                for user in users {
                    if let Some(key) = user_id(user) {
                        reuse_user(&mut profiles, &key, user);
                    }
                }
            }
        }
        for user in &item.like_users {
            if let Some(profile) = profiles.get_mut(&user.uid) {
                if profile.name.is_empty() {
                    profile.name = user.nickname.clone();
                }
                if profile.avatar_url.is_empty() {
                    profile.avatar_url = avatar_url(&user.avatar);
                }
            }
        }
    }
    if let Some(users)=doc.archive["users"].as_object(){for(key,user)in users{if let Some(profile)=profiles.get_mut(key){if profile.name.is_empty(){profile.name=user["name"].as_str().unwrap_or_default().into();}if profile.avatar_url.is_empty(){profile.avatar_url=user["avatar_resource_id"].as_str().and_then(|id|doc.archive["resources"][id]["url"].as_str()).map(avatar_url).unwrap_or_default();}}}}
    profiles
}
fn user_id(user: &Value) -> Option<String> {
    [&user["mid"], &user["uid"]]
        .into_iter()
        .map(raw_id)
        .find(|key| id(key))
}
fn reuse_user(profiles: &mut BTreeMap<String, Profile>, uid: &str, user: &Value) {
    let Some(profile) = profiles.get_mut(uid) else {
        return;
    };
    if profile.name.is_empty() {
        profile.name = [&user["nickname"], &user["uname"], &user["name"]]
            .into_iter()
            .filter_map(Value::as_str)
            .find(|s| !s.is_empty())
            .unwrap_or_default()
            .into();
    }
    if profile.avatar_url.is_empty() {
        profile.avatar_url = avatar(user);
    }
}
/// Add presentation metadata without changing the archive or its raw API payload.
pub fn item_summary(item: &crate::messages::Item, known: &BTreeMap<String, Profile>) -> Value {
    let mut value = crate::message_subjects::summary(item);
    if item.ty == "private" {
        value["sender_avatar_url"] = serde_json::json!(known
            .get(&item.sender_uid)
            .map(|p| p.avatar_url.as_str())
            .unwrap_or_default());
    }
    value
}
#[derive(Default)]
struct Cache {
    loaded: BTreeSet<String>,
    accounts: BTreeMap<String, BTreeMap<String, Profile>>,
    errors: BTreeMap<(String, String), String>,
    active: BTreeSet<(String, String)>,
    next_start: Option<Instant>,
}
#[derive(Default)]
pub struct Service {
    cache: Mutex<Cache>,
    changed: Condvar,
    root: Option<PathBuf>,
}
impl Service {
    fn file(&self, account: &str) -> Result<PathBuf> {
        if !id(account) {
            bail!("invalid account");
        }
        let root = match &self.root {
            Some(root) => root.clone(),
            None => crate::checkpoint::data_dir()?.join("cache/message-profiles"),
        };
        Ok(root.join(format!("{account}.json")))
    }
    fn load_account(&self, state: &mut Cache, account: &str) {
        if state.loaded.insert(account.into()) {
            if let Ok(path) = self.file(account) {
                if let Ok(bytes) = std::fs::read(path) {
                    if let Ok(saved) = serde_json::from_slice::<BTreeMap<String, Profile>>(&bytes) {
                        state.accounts.insert(account.into(), saved);
                    }
                }
            }
        }
    }
    /// Read all known local/cache data immediately, even while cold lookups are active.
    pub fn snapshot(
        &self,
        account: &str,
        mut local: BTreeMap<String, Profile>,
    ) -> BTreeMap<String, Profile> {
        if !id(account) {
            return local;
        }
        let mut state = self.cache.lock().unwrap();
        self.load_account(&mut state, account);
        if let Some(saved) = state.accounts.get(account) {
            for (key, profile) in &mut local {
                if let Some(cached) = saved.get(key).filter(|p| p.uid.as_str() == key.as_str()) {
                    if profile.name.is_empty() {
                        profile.name = cached.name.clone();
                    }
                    if profile.avatar_url.is_empty() {
                        profile.avatar_url = avatar_url(&cached.avatar_url);
                    }
                }
            }
        }
        local
    }
    pub fn summary(&self, doc: &Doc) -> Value {
        let known = self.snapshot(&doc.account_uid, local_profiles(doc));
        let mut summary = doc.summary();
        if let Some(sessions) = summary["sessions"].as_array_mut() {
            for session in sessions {
                if session["session_type"].as_u64() == Some(1) {
                    let photo = session["talker_id"]
                        .as_str()
                        .and_then(|key| known.get(key))
                        .map(|p| p.avatar_url.as_str())
                        .unwrap_or_default();
                    let photo = photo.to_owned();
                    session["avatar_url"] = serde_json::json!(photo);
                }
            }
        }
        summary
    }
    pub fn lookup(
        &self,
        account: &str,
        local: Profile,
        retry: bool,
        credentials: &Credentials,
        pacing: &Pacing,
    ) -> Profile {
        self.lookup_with(account, local, retry, pacing, |uid| {
            let cookie = credentials(account).ok_or_else(|| anyhow!("账号已切换或未登录"))?;
            let cookie = if cookie.contains('=') {
                cookie
            } else {
                format!("SESSDATA={cookie}")
            };
            let response = crate::bili::http_client()?
                .get(format!(
                    "https://api.bilibili.com/x/web-interface/card?mid={uid}"
                ))
                .timeout(Duration::from_secs(10))
                .header("Referer", "https://www.bilibili.com/")
                .header("Cookie", cookie)
                .send()?;
            if !response.status().is_success() {
                bail!("HTTP {}", response.status().as_u16());
            }
            let value: Value = response.json()?;
            if value["code"].as_i64() != Some(0) {
                bail!("API code {}", value["code"]);
            }
            let card = &value["data"]["card"];
            if raw_id(&card["mid"]) != uid {
                bail!("user mismatch");
            }
            let avatar_url = avatar_url(card["face"].as_str().unwrap_or_default());
            if avatar_url.is_empty() {
                bail!("头像暂不可用");
            }
            if credentials(account).is_none() {
                bail!("账号已切换");
            }
            Ok(Profile {
                uid: uid.into(),
                name: card["name"].as_str().unwrap_or_default().into(),
                avatar_url,
                error: String::new(),
            })
        })
    }
    fn lookup_with(
        &self,
        account: &str,
        mut local: Profile,
        retry: bool,
        pacing: &Pacing,
        fetch: impl FnOnce(&str) -> Result<Profile>,
    ) -> Profile {
        if !id(account) || !id(&local.uid) {
            local.error = "invalid user".into();
            return local;
        }
        let task = (account.to_owned(), local.uid.clone());
        let mut state = self.cache.lock().unwrap();
        self.load_account(&mut state, account);
        if retry {
            state.errors.remove(&task);
        }
        loop {
            if let Some(saved) = state.accounts.get(account).and_then(|a| a.get(&local.uid)) {
                if local.name.is_empty() {
                    local.name = saved.name.clone();
                }
                if local.avatar_url.is_empty() && saved.uid == local.uid {
                    local.avatar_url = avatar_url(&saved.avatar_url);
                }
            }
            if !local.avatar_url.is_empty() {
                local.error.clear();
                return local;
            }
            if let Some(error) = state.errors.get(&task) {
                local.error = error.clone();
                return local;
            }
            let wait = state
                .next_start
                .map(|at| at.saturating_duration_since(Instant::now()))
                .unwrap_or_default();
            if state.active.len() < 2 && !state.active.contains(&task) && wait.is_zero() {
                break;
            }
            state = if !wait.is_zero() {
                self.changed.wait_timeout(state, wait).unwrap().0
            } else {
                self.changed.wait(state).unwrap()
            };
        }
        state.active.insert(task.clone());
        let (min, max) = pacing();
        state.next_start =
            Some(Instant::now() + Duration::from_millis(crate::bili::random_delay_ms(min, max)));
        drop(state);
        let result = fetch(&local.uid);
        let mut state = self.cache.lock().unwrap();
        state.active.remove(&task);
        match result {
            Ok(found) if found.uid == local.uid && !avatar_url(&found.avatar_url).is_empty() => {
                if local.name.is_empty() {
                    local.name = found.name;
                }
                local.avatar_url = avatar_url(&found.avatar_url);
                local.error.clear();
                state
                    .accounts
                    .entry(account.into())
                    .or_default()
                    .insert(local.uid.clone(), local.clone());
                let _ = (|| -> Result<()> {
                    let path = self.file(account)?;
                    let parent = path.parent().ok_or_else(|| anyhow!("invalid cache"))?;
                    std::fs::create_dir_all(parent)?;
                    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
                    serde_json::to_writer(tmp.as_file_mut(), &state.accounts[account])?;
                    tmp.as_file().sync_all()?;
                    tmp.persist(path).map_err(|e| e.error)?;
                    Ok(())
                })();
            }
            _ => {
                local.error = "头像暂不可用，刷新可重试".into();
                state.errors.insert(task, local.error.clone());
            }
        }
        self.changed.notify_all();
        local
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn person(uid: &str) -> Profile {
        Profile {
            uid: uid.into(),
            ..Default::default()
        }
    }
    fn pacing() -> Pacing {
        Arc::new(|| (0, 0))
    }
    #[test]
    fn local_cached_failed_retry_and_account_isolation() {
        let dir = tempfile::tempdir().unwrap();
        let service = Service {
            root: Some(dir.path().into()),
            ..Default::default()
        };
        let photo = |uid: &str| {
            Ok(Profile {
                avatar_url: "https://i0.hdslb.com/a.jpg".into(),
                ..person(uid)
            })
        };
        let first = service.lookup_with("12", person("2"), false, &pacing(), photo);
        assert!(!first.avatar_url.is_empty());
        assert!(service
            .lookup_with("12", person("2"), false, &pacing(), |_| panic!("cached"))
            .error
            .is_empty());
        let reloaded = Service {
            root: Some(dir.path().into()),
            ..Default::default()
        };
        assert!(!reloaded
            .lookup_with("12", person("2"), false, &pacing(), |_| panic!(
                "disk cached"
            ))
            .avatar_url
            .is_empty());
        assert!(!service
            .lookup_with("13", person("2"), false, &pacing(), |_| bail!("offline"))
            .error
            .is_empty());
        assert!(!service
            .lookup_with("13", person("2"), false, &pacing(), |_| panic!(
                "failure cached"
            ))
            .error
            .is_empty());
        assert!(!service
            .lookup_with("13", person("2"), true, &pacing(), photo)
            .avatar_url
            .is_empty());
        let mut local = person("3");
        local.avatar_url = "https://i0.hdslb.com/local.jpg".into();
        assert!(service
            .lookup_with("12", local, false, &pacing(), |_| panic!("local"))
            .avatar_url
            .ends_with("local.jpg"));
    }
    #[test]
    fn rejects_wrong_user_and_unsafe_avatar() {
        let dir = tempfile::tempdir().unwrap();
        let service = Service {
            root: Some(dir.path().into()),
            ..Default::default()
        };
        assert!(!service
            .lookup_with("12", person("2"), false, &pacing(), |_| Ok(person("3")))
            .error
            .is_empty());
        assert!(!service
            .lookup_with("12", person("3"), false, &pacing(), |_| Ok(Profile {
                avatar_url: "javascript:bad".into(),
                ..person("3")
            }))
            .error
            .is_empty());
    }
    #[test]
    fn local_profiles_resolve_real_senders_and_preserve_archives() {
        let mut doc = Doc::new(12);
        doc.sessions.push(crate::messages::Session {
            key: "user_2".into(),
            talker_id: "2".into(),
            session_type: 1,
            name: "对方".into(),
            raw: serde_json::json!({"face":"//i0.hdslb.com/session.jpg"}),
            ..Default::default()
        });
        doc.sessions.push(crate::messages::Session {
            key: "group_4".into(),
            talker_id: "4".into(),
            session_type: 2,
            raw: serde_json::json!({"face":"https://example.test/group.jpg"}),
            ..Default::default()
        });
        doc.items.push(crate::messages::Item {
            ty: "private".into(),
            session: "group_4".into(),
            sender_uid: "3".into(),
            raw: serde_json::json!({"user":{"mid":"3","avatar":"https://i0.hdslb.com/member.jpg"}}),
            ..Default::default()
        });
        doc.items.push(crate::messages::Item {
            ty: "private".into(),
            session: "group_4".into(),
            sender_uid: "5".into(),
            raw: serde_json::json!({"user":{"mid":"99","avatar":"https://i0.hdslb.com/wrong.jpg"}}),
            ..Default::default()
        });
        let before = serde_json::to_string(&doc).unwrap();
        let profiles = local_profiles(&doc);
        assert!(profiles["2"].avatar_url.ends_with("session.jpg"));
        assert!(profiles["3"].avatar_url.ends_with("member.jpg"));
        assert!(!profiles.contains_key("4"));
        assert!(profiles["5"].avatar_url.is_empty());
        assert_eq!(before, serde_json::to_string(&doc).unwrap());
        assert!(!id("+2"));
        assert!(!id("../2"));
        assert!(!id("02"));
        assert!(id("18446744073709551614"));
    }
    #[test]
    fn concurrent_duplicates_share_one_lookup() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let service = Service {
            root: Some(dir.path().into()),
            ..Default::default()
        };
        let calls = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let service = &service;
                let calls = &calls;
                scope.spawn(move || {
                    let found = service.lookup_with("12", person("2"), false, &pacing(), |uid| {
                        calls.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(20));
                        Ok(Profile {
                            avatar_url: "https://i0.hdslb.com/a.jpg".into(),
                            ..person(uid)
                        })
                    });
                    assert!(!found.avatar_url.is_empty());
                });
            }
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn reuse_other_categories_without_widening_private_permissions() {
        let mut doc = Doc::new(12);
        for uid in ["2", "3", "4", "18446744073709551614"] {
            doc.sessions.push(crate::messages::Session {
                key: format!("user_{uid}"),
                talker_id: uid.into(),
                session_type: 1,
                ..Default::default()
            });
        }
        doc.items.push(crate::messages::Item {
            ty: "replies".into(),
            sender_uid: "2".into(),
            raw: serde_json::json!({"user":{"mid":"2","avatar":"https://i0.hdslb.com/reply.jpg"}}),
            ..Default::default()
        });
        doc.items.push(crate::messages::Item {
            ty: "mentions".into(),
            sender_uid: "3".into(),
            raw: serde_json::json!({"user":{"mid":"3","face":"//i0.hdslb.com/mention.jpg"}}),
            ..Default::default()
        });
        doc.items.push(crate::messages::Item { ty: "likes".into(), raw: serde_json::json!({"users":[{"mid":"4","avatar":"https://i0.hdslb.com/like.jpg"},{"mid":"99","avatar":"https://i0.hdslb.com/other.jpg"}]}), like_users: vec![crate::messages::LikeUser { uid: "18446744073709551614".into(), avatar: "https://i0.hdslb.com/detail.jpg".into(), ..Default::default() }], ..Default::default() });
        let before = serde_json::to_string(&doc).unwrap();
        let known = local_profiles(&doc);
        assert!(known["2"].avatar_url.ends_with("reply.jpg"));
        assert!(known["3"].avatar_url.ends_with("mention.jpg"));
        assert!(known["4"].avatar_url.ends_with("like.jpg"));
        assert!(known["18446744073709551614"]
            .avatar_url
            .ends_with("detail.jpg"));
        assert!(!known.contains_key("99"));
        assert_eq!(before, serde_json::to_string(&doc).unwrap());
    }
    #[test]
    fn snapshot_and_page_metadata_do_not_wait_for_cold_queries() {
        let dir = tempfile::tempdir().unwrap();
        let service = Service {
            root: Some(dir.path().into()),
            ..Default::default()
        };
        let mut doc = Doc::new(12);
        doc.sessions.push(crate::messages::Session {
            key: "user_2".into(),
            talker_id: "2".into(),
            session_type: 1,
            ..Default::default()
        });
        doc.items.push(crate::messages::Item {
            id: "1".into(),
            ty: "private".into(),
            sender_uid: "2".into(),
            session: "user_2".into(),
            ..Default::default()
        });
        service.lookup_with("12", person("2"), false, &pacing(), |uid| {
            Ok(Profile {
                avatar_url: "https://i0.hdslb.com/cache.jpg".into(),
                ..person(uid)
            })
        });
        {
            let mut cache = service.cache.lock().unwrap();
            cache.active.insert(("12".into(), "3".into()));
            cache.next_start = Some(Instant::now() + Duration::from_secs(60));
        }
        let before = serde_json::to_string(&doc).unwrap();
        let known = service.snapshot("12", local_profiles(&doc));
        assert!(known["2"].avatar_url.ends_with("cache.jpg"));
        assert!(service.summary(&doc)["sessions"][0]["avatar_url"]
            .as_str()
            .unwrap()
            .ends_with("cache.jpg"));
        let item = item_summary(&doc.items[0], &known);
        assert!(item["sender_avatar_url"]
            .as_str()
            .unwrap()
            .ends_with("cache.jpg"));
        assert!(item["raw"].is_null());
        assert_eq!(before, serde_json::to_string(&doc).unwrap());
        assert!(service.snapshot("99", local_profiles(&doc))["2"]
            .avatar_url
            .is_empty());
        let reloaded = Service {
            root: Some(dir.path().into()),
            ..Default::default()
        };
        assert!(reloaded.snapshot("12", local_profiles(&doc))["2"]
            .avatar_url
            .ends_with("cache.jpg"));
    }
}
