//! Scope-local catalogue for preview images. Archive records are never rebuilt to serve an asset.
use crate::archive;
use serde_json::{json, Map, Value};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Scope {
    root: PathBuf,
    account: String,
}

impl Scope {
    pub fn new(root: PathBuf, uid: &str) -> Option<Self> {
        let account = if uid == "0" { "public" } else { uid };
        if account != "public"
            && (account.is_empty() || !account.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return None;
        }
        Some(Self {
            root,
            account: account.into(),
        })
    }

    fn directory(&self) -> PathBuf {
        self.root.join("resources").join(&self.account)
    }

    pub fn visible_to(&self, uid: &str) -> bool {
        uid.is_empty() || self.account == "public" || self.account == uid
    }

    pub fn token(&self) -> String {
        archive::hash(&format!("{}:{}", self.account, self.root.display()))
    }

    fn open(&self, filename: &str) -> Option<(File, &'static str)> {
        // Check the actual account directory again at the point of opening, including junctions.
        let root = self.root.canonicalize().ok()?;
        let expected = root.join("resources").join(&self.account);
        let dir = self.directory().canonicalize().ok()?;
        if dir != expected {
            return None;
        }
        let file = dir.join(filename).canonicalize().ok()?;
        if file.parent() != Some(dir.as_path()) || !file.is_file() {
            return None;
        }
        Some((File::open(&file).ok()?, crate::messages::media_type(&file)))
    }
}

fn resource_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

pub struct Index {
    token: String,
    files: BTreeMap<String, String>,
    urls: Map<String, Value>,
}

impl Index {
    fn read(scope: &Scope) -> Self {
        let token = scope.token();
        let dir = scope.directory();
        let mut files = BTreeMap::new();
        if let Ok(entries) = fs::read_dir(&dir) {
            let mut entries: Vec<_> = entries.flatten().collect();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let name = entry.file_name();
                let Some(filename) = name.to_str() else {
                    continue;
                };
                let Some(stem) = Path::new(filename)
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                else {
                    continue;
                };
                if resource_id(stem) && entry.file_type().is_ok_and(|kind| kind.is_file()) {
                    files
                        .entry(stem.to_owned())
                        .or_insert_with(|| filename.to_owned());
                }
            }
        }
        let mut index = Self {
            token,
            files,
            urls: Map::new(),
        };
        if let Ok(data) = fs::read(dir.join("index.json")) {
            if let Ok(entries) = serde_json::from_slice::<Map<String, Value>>(&data) {
                index.urls = index.urls_for(entries.values());
            }
        }
        index
    }

    fn url_for_id(&self, id: &str) -> Option<String> {
        self.files
            .contains_key(id)
            .then(|| format!("/api/archive-resources/{id}?scope={}", self.token))
    }

    fn url_for_source(&self, source: &str) -> Option<String> {
        if let Some(local) = self.urls.get(source).and_then(Value::as_str) {
            return Some(local.to_owned());
        }
        let normalized = crate::message_subjects::image_url(source);
        if normalized.is_empty() {
            return None;
        }
        self.url_for_id(&archive::hash(&normalized))
    }

    pub fn resource_images(&self, saved: Option<&Map<String, Value>>) -> Map<String, Value> {
        let mut urls = self.urls.clone();
        if let Some(saved) = saved {
            urls.extend(self.urls_for(saved.values()));
        }
        urls
    }

    fn urls_for<'a>(&self, records: impl Iterator<Item = &'a Value>) -> Map<String, Value> {
        records
            .filter_map(|record| {
                let source = record["url"].as_str()?;
                let id = record["id"].as_str()?;
                Some((source.to_owned(), json!(self.url_for_id(id)?)))
            })
            .collect()
    }

    pub fn localize(&self, value: &mut Value, urls: &mut Map<String, Value>) {
        match value {
            Value::Object(fields) => {
                let mut original = Map::new();
                for (key, field) in fields.iter_mut() {
                    if [
                        "avatar",
                        "avatar_url",
                        "face",
                        "cover",
                        "cover_src",
                        "image",
                        "src",
                        "img_src",
                        "voice_url",
                        "audio_url",
                        "file_url",
                    ]
                    .contains(&key.as_str())
                    {
                        if let Some(source) = field.as_str() {
                            if let Some(local) = self.url_for_source(source) {
                                original.insert(key.clone(), json!(source));
                                urls.insert(source.to_owned(), json!(local));
                                *field = json!(local);
                            }
                        }
                    } else if ["pictures", "text_pics"].contains(&key.as_str()) {
                        if let Some(values) = field.as_array() {
                            for source in values.iter().filter_map(Value::as_str) {
                                if let Some(local) = self.url_for_source(source) {
                                    urls.insert(source.to_owned(), json!(local));
                                }
                            }
                        }
                    } else if !matches!(
                        key.as_str(),
                        "raw" | "resource_images" | "_original_resources"
                    ) {
                        self.localize(field, urls);
                    }
                }
                if !original.is_empty() {
                    fields.insert("_original_resources".into(), Value::Object(original));
                }
            }
            Value::Array(items) => {
                for item in items {
                    self.localize(item, urls);
                }
            }
            _ => {}
        }
    }

    pub fn open(&self, scope: &Scope, id: &str) -> Option<(File, &'static str)> {
        scope.open(self.files.get(id)?)
    }
}

#[derive(PartialEq)]
struct Stamp {
    directory: Option<SystemTime>,
    index: Option<(SystemTime, u64)>,
}
impl Stamp {
    fn read(scope: &Scope) -> Self {
        let dir = scope.directory();
        Self {
            directory: fs::metadata(&dir).and_then(|m| m.modified()).ok(),
            index: fs::metadata(dir.join("index.json"))
                .ok()
                .and_then(|m| Some((m.modified().ok()?, m.len()))),
        }
    }
}

#[derive(Default)]
pub struct Cache {
    indexes: Mutex<BTreeMap<Scope, (Stamp, Arc<Index>)>>,
}

impl Cache {
    pub fn get(&self, scope: &Scope) -> Arc<Index> {
        let stamp = Stamp::read(scope);
        if let Some((saved, index)) = self.indexes.lock().unwrap().get(scope) {
            if *saved == stamp {
                return index.clone();
            }
        }
        // Disk reads for one cold account should not block another account's cached images.
        let index = Arc::new(Index::read(scope));
        if Stamp::read(scope) != stamp {
            return index;
        }
        let mut indexes = self.indexes.lock().unwrap();
        if let Some((saved, cached)) = indexes.get(scope) {
            if *saved == stamp {
                return cached.clone();
            }
        }
        if indexes.len() >= 8 && !indexes.contains_key(scope) {
            indexes.pop_first();
        }
        indexes.insert(scope.clone(), (stamp, index.clone()));
        index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_index_reuses_existing_files_and_keeps_accounts_isolated() {
        let tmp = tempfile::tempdir().unwrap();
        let one = Scope::new(tmp.path().to_path_buf(), "12").unwrap();
        let two = Scope::new(tmp.path().to_path_buf(), "13").unwrap();
        let source = "https://i0.hdslb.com/cover.png";
        let id = archive::hash(source);
        fs::create_dir_all(one.directory()).unwrap();
        fs::write(one.directory().join(format!("{id}.png")), b"image").unwrap();
        let cache = Cache::default();
        let index = cache.get(&one);
        assert!(Arc::ptr_eq(&index, &cache.get(&one)));
        assert!(index.open(&one, &id).is_some());
        assert!(cache.get(&two).open(&two, &id).is_none());
        let mut view = json!({"cover":source,"pictures":[source]});
        let mut images = Map::new();
        index.localize(&mut view, &mut images);
        assert_eq!(view["_original_resources"]["cover"], source);
        assert_eq!(images[source], view["cover"]);
        fs::remove_file(one.directory().join(format!("{id}.png"))).unwrap();
        assert!(index.open(&one, &id).is_none());
        fs::write(one.directory().join("index.json"), "{}").unwrap();
        assert!(!Arc::ptr_eq(&index, &cache.get(&one)));
    }
}
