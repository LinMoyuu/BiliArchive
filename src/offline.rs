//! Persistent offline libraries and portable, credential-free account packages.
use crate::*;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Account {
    pub library_id: String,
    pub uid: u64,
    pub name: String,
    #[serde(default)]
    pub avatar: String,
    #[serde(default)]
    pub avatar_id: String,
    #[serde(default)]
    pub avatar_url: String,
}
impl Account {
    fn key(&self) -> String {
        format!("{}:{}", self.library_id, self.uid)
    }
}
#[derive(Serialize, Deserialize)]
struct Catalog {
    #[serde(default = "catalog_version")]
    version: u32,
    #[serde(default)]
    accounts: Vec<Account>,
    #[serde(default)]
    active: Option<String>,
}
fn catalog_version() -> u32 {
    1
}
impl Default for Catalog {
    fn default() -> Self {
        Self {
            version: 1,
            accounts: vec![],
            active: None,
        }
    }
}
pub enum Action {
    Import(Vec<PathBuf>),
    Select(String),
    Export(PathBuf),
    Restore,
}
fn catalog_path() -> Result<PathBuf> {
    Ok(checkpoint::data_dir()?.join("libraries/catalog.json"))
}
fn catalog() -> Result<Catalog> {
    let p = catalog_path()?;
    if !p.exists() {
        return Ok(Catalog::default());
    }
    let c: Catalog =
        serde_json::from_slice(&std::fs::read(p)?).context("离线资料库目录损坏，未覆盖")?;
    anyhow::ensure!(c.version == 1, "离线资料库目录版本不支持，未覆盖");
    Ok(c)
}
fn save_catalog(c: &Catalog) -> Result<()> {
    secure::write_config(&catalog_path()?, &serde_json::to_value(c)?)
}
fn local_root(id: &str) -> Result<PathBuf> {
    anyhow::ensure!(library::valid_id(id), "资料库 ID 无效");
    Ok(checkpoint::data_dir()?.join("libraries").join(id))
}
pub fn has_saved_selection() -> bool {
    catalog().is_ok_and(|c| c.active.is_some())
}
pub fn publish_catalog(ui: &Weak<AppWindow>) -> Result<()> {
    let rows = catalog()?
        .accounts
        .into_iter()
        .map(|a| OfflineAccountRow {
            key: a.key().into(),
            label: format!(
                "{} · {}",
                a.name,
                if a.uid == 0 {
                    "公共".into()
                } else {
                    a.uid.to_string()
                }
            )
            .into(),
        })
        .collect::<Vec<_>>();
    emit(ui, move |w| {
        w.set_offline_accounts(ModelRc::new(VecModel::from(rows)))
    });
    Ok(())
}
pub fn forget_active() -> Result<()> {
    let mut c = catalog()?;
    c.active = None;
    save_catalog(&c)?;
    library::set_active(None);
    Ok(())
}
pub fn leave(shared: &Arc<Shared>, ui: &Weak<AppWindow>) {
    if !shared.offline.swap(false, Ordering::SeqCst) {
        return;
    }
    if let Some(mut active) = library::active() {
        active.offline = false;
        library::set_active(Some(active));
    }
    shared.account_uid.store(0, Ordering::SeqCst);
    archive::set_account(0);
    *shared.doc.lock().unwrap() = ArchiveDoc::default();
    *shared.messages.lock().unwrap() = messages::Store::default();
    shared.rows.lock().unwrap().clear();
    *shared.public_account.lock().unwrap() = account_ui::PublicAccount::default();
    // Keep the selected library when the original account is verified online; its checkpoints and NG remain usable.
    emit(ui, |w| {
        w.set_account_offline(false);
        w.set_account_logged_in(false);
        w.set_account_uid("".into());
        w.set_account_name("".into());
        w.set_account_has_avatar(false);
        w.set_message_ng_count(0);
        w.set_message_has_resume(false);
        message_dates::reset(w);
    });
    refresh_rows(shared, ui);
    refresh_stats(shared, ui);
}
fn safe_files(root: &Path) -> Result<Vec<PathBuf>> {
    scan_files(root, |_| true)
}

fn scan_files(root: &Path, include: impl Fn(&Path) -> bool) -> Result<Vec<PathBuf>> {
    let mut files = vec![];
    if !root.exists() {
        return Ok(files);
    }
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if !include(&path) {
                continue;
            }
            let meta = std::fs::symlink_metadata(&path)?;
            anyhow::ensure!(!meta.file_type().is_symlink(), "资料目录中含有链接，未跟随");
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                anyhow::ensure!(
                    meta.file_attributes() & 0x400 == 0,
                    "资料目录中含有重解析链接"
                );
            }
            if meta.is_dir() {
                dirs.push(path);
            } else if meta.is_file() {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}
fn read_comments(root: &Path, uid: u64, workers: usize) -> Result<ArchiveDoc> {
    let scope = if uid == 0 {
        "public".into()
    } else {
        uid.to_string()
    };
    let files = safe_files(&archive::root(root).join(scope).join("comments"))?
        .into_iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect::<Vec<_>>();
    let mut documents = Vec::new();
    for batch in files.chunks(32) {
        for result in crate::local_read::map_ordered(batch, workers, |path| -> Result<ArchiveDoc> {
            let value = archive::Envelope::parse(&std::fs::read(path)?)?;
            anyhow::ensure!(archive_uid(&value) == uid, "评论归档账号不匹配");
            archive::comments(&value).with_context(|| path.display().to_string())
        }) {
            documents.push(result?);
        }
    }
    Ok(combine_comments(root, uid, documents))
}
fn archive_uid(value: &archive::Envelope) -> u64 {
    value.meta["account_uid"]
        .as_str()
        .and_then(|uid| uid.parse().ok())
        .unwrap_or(0)
}
fn combine_comments(root: &Path, uid: u64, documents: Vec<ArchiveDoc>) -> ArchiveDoc {
    let mut doc = ArchiveDoc::default();
    let mut resources = serde_json::Map::new();
    let mut videos = Vec::new();
    for mut document in documents {
        if let Some(table) = document
            .archive
            .get_mut("resources")
            .and_then(Value::as_object_mut)
        {
            resources.append(table);
        }
        videos.append(&mut document.videos);
    }
    doc.archive = json!({
        "resource_root": archive::root(root),
        "account_uid": (uid != 0).then(|| uid.to_string()),
        "resources": resources,
    });
    doc.merge_videos(videos);
    doc
}
fn activate(account: &Account, shared: &Arc<Shared>, ui: &Weak<AppWindow>) -> Result<()> {
    let root = local_root(&account.library_id)?;
    let manifest = library::ensure(&root)?;
    anyhow::ensure!(
        manifest.library_id == account.library_id,
        "资料库 ID 不匹配"
    );
    let _reading = LocalReadGuard::begin(shared, ui);
    set_foot(ui, "正在后台读取本地评论和消息…".into());
    let (precache, workers) = {
        let cfg = shared.cfg.lock().unwrap();
        (cfg.archive_precache, cfg.archive_workers())
    };
    let comments = if precache && crate::preview_disk::small_comment_archive(&root, account.uid) {
        read_comments(&root, account.uid, workers)?
    } else {
        crate::preview_disk::desktop_projection(&root, account.uid)
    };
    activate_loaded(account, shared, ui, root, comments, messages::Store::default())?;
    if precache && account.uid != 0 {
        messages_ui::load_overview(shared, ui, account.uid);
    }
    log_foot(shared, ui, format!("离线账号 {} · 本地资料已读取", account.name));
    Ok(())
}

fn load_account_messages(root: &Path, uid: u64, workers: usize) -> Result<messages::Store> {
    if uid == 0 {
        Ok(messages::Store::default())
    } else {
        messages::load_with_workers(root, uid, workers)
    }
}

fn activate_loaded(
    account: &Account,
    shared: &Arc<Shared>,
    ui: &Weak<AppWindow>,
    root: PathBuf,
    comments: ArchiveDoc,
    messages: messages::Store,
) -> Result<()> {
    // Invalidate old online responses before changing the visible documents.
    shared.qr_gen.fetch_add(1, Ordering::SeqCst);
    shared.offline.store(true, Ordering::SeqCst);
    library::set_active(Some(library::Active {
        id: account.library_id.clone(),
        root: root.clone(),
        offline: true,
    }));
    shared.account_uid.store(account.uid, Ordering::SeqCst);
    archive::set_account(account.uid);
    shared.rows.lock().unwrap().clear();
    shared.rows.lock().unwrap().extend(
        comments
            .videos
            .iter()
            .map(|video| (video.without_comments(), false)),
    );
    *shared.doc.lock().unwrap() = comments;
    *shared.messages.lock().unwrap() = messages;
    *shared.checkpoint.lock().unwrap() = None;
    let uid = account.uid.to_string();
    let avatar_url = if account.avatar_id.is_empty() {
        String::new()
    } else {
        format!("/api/archive-resources/{}", account.avatar_id)
    };
    *shared.public_account.lock().unwrap() = account_ui::PublicAccount {
        logged_in: false,
        uid: uid.clone(),
        name: account.name.clone(),
        avatar_url,
        avatar_fallback_url: String::new(),
        mode: "offline".into(),
        library_id: account.library_id.clone(),
    };
    let name = account.name.clone();
    emit(ui, move |w| {
        w.set_account_offline(true);
        w.set_account_logged_in(false);
        w.set_account_uid(uid.into());
        w.set_account_name(name.into());
        w.set_account_has_avatar(false);
        w.set_scope(1);
        message_dates::reset(w);
        w.set_has_resume(false);
        w.set_qr_visible(false);
    });
    refresh_offline_avatar(account, &root, shared, ui);
    refresh_rows(shared, ui);
    refresh_stats(shared, ui);
    messages_ui::refresh(shared, ui);
    let mut c = catalog()?;
    let key = account.key();
    if c.active.as_deref() != Some(key.as_str()) {
        c.active = Some(key);
        save_catalog(&c)?;
    }
    Ok(())
}
fn refresh_offline_avatar(
    account: &Account,
    root: &Path,
    shared: &Arc<Shared>,
    ui: &Weak<AppWindow>,
) {
    if account.avatar_id.is_empty() && account.avatar_url.is_empty() {
        return;
    }
    let resource_root = archive::root(root);
    let indexed = std::fs::read(
        resource_root
            .join("resources")
            .join(account.uid.to_string())
            .join("index.json"),
    )
    .ok()
    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    .map(|index| index[&account.avatar_id].clone())
    .unwrap_or_default();
    let comment_resource =
        shared.doc.lock().unwrap().archive["resources"][&account.avatar_id].clone();
    let message_resource =
        shared.messages.lock().unwrap().doc.archive["resources"][&account.avatar_id].clone();
    let resources = [&indexed, &comment_resource, &message_resource];
    let remote = std::iter::once(account.avatar_url.as_str())
        .chain(
            resources
                .iter()
                .filter_map(|resource| resource["url"].as_str()),
        )
        .find_map(account_ui::public_image_url)
        .unwrap_or_default();
    let local = std::iter::once(account.avatar.as_str())
        .chain(
            resources
                .iter()
                .filter_map(|resource| resource["path"].as_str()),
        )
        .find_map(|path| archive::resource_file(&resource_root, path));
    {
        let mut public = shared.public_account.lock().unwrap();
        if public.avatar_fallback_url == remote && local.is_none() {
            return;
        }
        public.avatar_fallback_url = remote.clone();
    }
    account_ui::show_archive_avatar(
        shared,
        ui,
        account.library_id.clone(),
        account.uid,
        local,
        remote,
    );
}

fn profile(env: &archive::Envelope, id: &str) -> Account {
    let uid = env.meta["account_uid"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let user = env
        .users
        .get(&uid.to_string())
        .cloned()
        .unwrap_or(Value::Null);
    let name = ["name", "nickname", "uname"]
        .iter()
        .find_map(|k| user[*k].as_str().filter(|s| !s.trim().is_empty()))
        .map(str::to_owned)
        .unwrap_or_else(|| {
            if uid == 0 {
                "公共归档".into()
            } else {
                format!("用户 {uid}")
            }
        });
    let avatar_id = user["avatar_resource_id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let avatar = env
        .resources
        .get(&avatar_id)
        .and_then(|r| r["path"].as_str())
        .unwrap_or_default()
        .to_owned();
    Account {
        library_id: id.into(),
        uid,
        name,
        avatar,
        avatar_url: env
            .resources
            .get(&avatar_id)
            .and_then(|resource| resource["url"].as_str())
            .and_then(account_ui::public_image_url)
            .unwrap_or_default(),
        avatar_id,
    }
}
fn source_manifest(path: &Path) -> Result<Option<library::Manifest>> {
    for parent in path.ancestors() {
        let marker = if parent.file_name().is_some_and(|n| n == "archive-v1") {
            parent.join(".library.json")
        } else {
            parent.join("archive-v1/.library.json")
        };
        if marker.is_file() {
            let m: library::Manifest = serde_json::from_slice(&std::fs::read(marker)?)?;
            anyhow::ensure!(
                m.format == "bili-comment-hub-library"
                    && m.version == 1
                    && library::valid_id(&m.library_id),
                "导入资料库标识无效"
            );
            return Ok(Some(m));
        }
    }
    Ok(None)
}
fn state_allowed(name: &str, uid: u64) -> bool {
    let u = uid.to_string();
    name == format!("checkpoints/messages/v2/{u}.json")
        || name == format!("checkpoints/messages/v2/{u}.ng.json")
        || name == format!("checkpoints/messages/v2/{u}.interactions.json")
        || name == format!("checkpoints/messages/v2/{u}.interactions.ng.json")
        || name == format!("ng/messages/v1/{u}.paging.json")
        || name == format!("ng/messages/v1/{u}.interactions.paging.json")
        || name.starts_with(&format!("checkpoints/{u}/")) && name.ends_with(".json")
}
fn validate_state(name: &str, value: &Value, uid: u64, library_id: &str) -> Result<()> {
    if name.starts_with("checkpoints/messages/") {
        messages::validate_portable_task(value.clone(), uid, library_id)?;
    } else if name.starts_with("ng/") {
        messages::validate_portable_ng(value.clone(), uid, library_id)?;
    } else {
        let task: checkpoint::Task =
            serde_json::from_value(value.clone()).context("评论断点内容无效")?;
        anyhow::ensure!(
            task.version == 3 && task.uid == uid,
            "评论断点版本或账号不匹配"
        );
    }
    Ok(())
}
fn import(paths: Vec<PathBuf>, shared: &Arc<Shared>, ui: &Weak<AppWindow>) -> Result<()> {
    let started = std::time::Instant::now();
    anyhow::ensure!(!paths.is_empty(), "未选择导入资料");
    let base = checkpoint::data_dir()?.join("libraries");
    std::fs::create_dir_all(&base)?;
    let stage = tempfile::tempdir_in(&base)?;
    let mut candidates = vec![];
    let mut state_source = None;
    let mut manifest = None;
    let mut strict = false;
    let workers = shared.cfg.lock().unwrap().archive_workers();
    let _reading = LocalReadGuard::begin(shared, ui);
    if paths.len() == 1
        && paths[0]
            .extension()
            .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case("zip"))
    {
        let unpack = stage.path().join("unpack");
        set_foot(ui, "导入资料包：正在解包…".into());
        portable_zip::extract_with_workers(&paths[0], &unpack, workers)?;
        let marker = unpack.join("archive-v1/.library.json");
        let package_manifest: library::Manifest =
            serde_json::from_slice(&std::fs::read(&marker).context("离线资料包缺少资料库清单")?)?;
        anyhow::ensure!(
            package_manifest.format == "bili-comment-hub-library"
                && package_manifest.version == 1
                && library::valid_id(&package_manifest.library_id),
            "资料包标识无效"
        );
        manifest = Some(package_manifest);
        candidates = safe_files(&unpack.join("archive-v1"))?;
        state_source = Some(unpack);
    } else {
        for path in &paths {
            if path.is_dir() {
                candidates.extend(safe_files(path)?);
                if manifest.is_none() {
                    manifest = source_manifest(path)?;
                }
            } else {
                anyhow::ensure!(
                    path.extension().is_some_and(|s| s == "json"),
                    "请单独导入 ZIP 或选择新版 JSON"
                );
                candidates.push(path.clone());
                strict = true;
                if manifest.is_none() {
                    manifest = source_manifest(path)?;
                }
            }
        }
    }
    if state_source.is_none() {
        for path in &candidates {
            if path.file_name().is_some_and(|s| s == ".library.json") {
                let found: library::Manifest = serde_json::from_slice(&std::fs::read(path)?)?;
                anyhow::ensure!(
                    found.format == "bili-comment-hub-library"
                        && found.version == 1
                        && library::valid_id(&found.library_id),
                    "资料库标识无效"
                );
                if let Some(ref expected) = manifest {
                    anyhow::ensure!(
                        expected.library_id == found.library_id,
                        "请选择单个资料库，不能跨库合并"
                    );
                } else {
                    manifest = Some(found);
                }
            } else if path.extension().is_some_and(|s| s == "json") {
                if let Some(found) = source_manifest(path)? {
                    if let Some(ref expected) = manifest {
                        anyhow::ensure!(
                            expected.library_id == found.library_id,
                            "不能把不同资料库合并为同一身份"
                        );
                    } else {
                        manifest = Some(found);
                    }
                }
            }
        }
    }
    let target = stage.path().join("library");
    std::fs::create_dir_all(&target)?;
    let manifest = match manifest {
        Some(m) => m,
        None => library::ensure(&target)?,
    };
    let destination = local_root(&manifest.library_id)?;
    let new_library = !destination.exists();
    if !new_library {
        anyhow::ensure!(
            library::ensure(&destination)?.library_id == manifest.library_id,
            "本地资料库身份不匹配，未覆盖"
        );
        for file in safe_files(&destination)? {
            let rel = file.strip_prefix(&destination)?;
            let out = target.join(rel);
            std::fs::create_dir_all(out.parent().unwrap())?;
            let immutable_resource = rel.starts_with(Path::new("archive-v1").join("resources"))
                && rel.file_name().is_some_and(|name| name != "index.json");
            if !immutable_resource || std::fs::hard_link(&file, &out).is_err() {
                std::fs::copy(file, out)?;
            }
        }
    }
    secure::write_config(
        &target.join("archive-v1/.library.json"),
        &serde_json::to_value(&manifest)?,
    )?;
    set_foot(ui, "导入资料包：正在校验并合并归档…".into());
    let mut accounts = BTreeMap::new();
    let mut reports = Vec::new();
    let mut collection_times = Vec::new();
    let mut my_comments = Vec::new();
    let mut count = 0;
    let mut resource_copies = archive::ResourceCopies::default();
    let mut message_files: BTreeMap<u64, Vec<messages::Doc>> = BTreeMap::new();
    let mut message_groups: BTreeMap<u64, BTreeSet<String>> = BTreeMap::new();
    let mut comment_files: BTreeMap<u64, Vec<ArchiveDoc>> = BTreeMap::new();
    let mut comments_to_write = Vec::new();
    let mut reuse_comment_view = new_library;
    let archive_target = archive::root(&target);
    let staged_archive = state_source.as_ref().map(|root| root.join("archive-v1"));
    candidates.retain(|path| {
        path.extension().is_some_and(|ext| ext == "json")
            && !path.file_name().is_some_and(|name| name == ".library.json" || name == "index.json")
        // Resources can themselves be JSON attachments.
            && !staged_archive.as_ref().is_some_and(|root| path.starts_with(root.join("resources")))
    });
    for batch in candidates.chunks(16) {
        let decoded =
            crate::local_read::map_ordered(batch, workers, |path| -> Result<(Vec<u8>, Value)> {
                let bytes = std::fs::read(path)?;
                let parsed =
                    serde_json::from_slice(&bytes).with_context(|| path.display().to_string())?;
                Ok((bytes, parsed))
            });
        for (path, entry) in batch.iter().zip(decoded) {
            let (bytes, parsed) = entry?;
            if !strict
                && path
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|name| name == "reports")
            {
                let uid = collection_report_uid(&parsed)?;
                if let Some(root) = &staged_archive {
                    let expected = root.join(uid.to_string()).join("reports");
                    anyhow::ensure!(
                        path.parent() == Some(expected.as_path()),
                        "资料包报告目录与账号不匹配"
                    );
                }
                reports.push((uid, path.clone()));
                continue;
            }
            if let Some(root) = &staged_archive {
                if path.file_name().is_some_and(|name| name == crate::collection_time::FILE_NAME) {
                    let stamp = crate::collection_time::Stamp::parse(parsed)?;
                    anyhow::ensure!(path == &crate::collection_time::path(root, stamp.uid), "资料包采集时间与账号不匹配");
                    collection_times.push(stamp);
                    continue;
                }
            }
            if parsed["format"].as_str().is_some_and(crate::my_comments::Detail::recognizes_format) {
                let detail: crate::my_comments::Detail = serde_json::from_value(parsed)?;
                detail.validate()?;
                if let Some(root) = &staged_archive {
                    let expected = crate::my_comments::path(
                        &root.join(detail.account_uid.to_string()).join("messages"), &detail.rpid,
                    );
                    anyhow::ensure!(path == &expected, "我的评论归档目录与账号不匹配");
                }
                my_comments.push(detail);
                continue;
            }
            if parsed["format"] != "bili-archive" {
                if strict {
                    bail!("仅支持 Bili Archive v1 JSON");
                }
                continue;
            }
            let mut env = archive::Envelope::from_value(parsed)?;
            let source = archive::storage_root(path.parent().context("归档文件缺少目录")?);
            let staged_source = staged_archive.as_ref().is_some_and(|root| root == &source);
            if staged_source {
                let uid = archive_uid(&env);
                let scope = if uid == 0 {
                    "public".into()
                } else {
                    uid.to_string()
                };
                anyhow::ensure!(
                    path.starts_with(source.join(scope)),
                    "资料包归档目录与账号不匹配"
                );
            }
            let normalized =
                resource_copies.copy(&mut env, &source, &archive_target, staged_source)?;
            let direct = new_library && staged_source && !normalized;
            let account = profile(&env, &manifest.library_id);
            let uid = account.uid;
            accounts
                .entry(uid)
                .and_modify(|old: &mut Account| {
                    if old.name == format!("用户 {uid}") {
                        old.name = account.name.clone();
                    }
                    if old.avatar_url.is_empty() {
                        old.avatar_url = account.avatar_url.clone();
                    }
                    if old.avatar_id.is_empty() {
                        old.avatar_id = account.avatar_id.clone();
                        old.avatar = account.avatar.clone();
                    }
                })
                .or_insert(account);
            let relative = path.strip_prefix(&source).ok();
            if env.source["type"] == "messages" {
                let doc = archive::messages(&env)?;
                doc.validate(uid)?;
                let mut groups: BTreeSet<String> = doc
                    .sessions
                    .iter()
                    .map(|session| session.key.clone())
                    .collect();
                groups.extend(
                    doc.items
                        .iter()
                        .filter(|item| item.ty != "private")
                        .map(|item| item.ty.clone()),
                );
                // Empty category archives still have a group and should round-trip.
                if groups.is_empty() {
                    if let Some(category) = env.source["category"]
                        .as_str()
                        .filter(|name| messages::TYPES.contains(name) && *name != "private")
                    {
                        groups.insert(category.to_owned());
                    }
                }
                let message_root = messages::base(&target, uid);
                let canonical_path = if groups.len() == 1 {
                    let group = groups.first().unwrap();
                    Some(if messages::TYPES.contains(&group.as_str()) {
                        message_root.join(format!("{group}.json"))
                    } else {
                        message_root.join("private").join(format!("{group}.json"))
                    })
                } else {
                    None
                };
                if direct
                    && canonical_path.as_ref().is_some_and(|dest| {
                        relative
                            .is_some_and(|rel| dest.strip_prefix(&archive_target).ok() == Some(rel))
                    })
                {
                    move_import_file(path, &canonical_path.unwrap())?;
                } else {
                    message_groups.entry(uid).or_default().extend(groups);
                }
                message_files.entry(uid).or_default().push(doc);
            } else {
                let doc = archive::comments(&env)?;
                let canonical_path = if doc.videos.len() == 1 {
                    export::item_path_for(&target, &doc.videos[0], uid).ok()
                } else {
                    None
                };
                if direct
                    && canonical_path.as_ref().is_some_and(|dest| {
                        relative
                            .is_some_and(|rel| dest.strip_prefix(&archive_target).ok() == Some(rel))
                    })
                {
                    move_import_file(path, &canonical_path.unwrap())?;
                } else {
                    reuse_comment_view = false;
                    if doc.videos.iter().any(|video| video.owner_mid == 0) {
                        let account_dir = if uid == 0 {
                            "public".into()
                        } else {
                            uid.to_string()
                        };
                        let file = archive_target
                            .join(account_dir)
                            .join("comments/public")
                            .join(format!(
                                "{}.json",
                                archive::hash(&String::from_utf8_lossy(&bytes))
                            ));
                        archive::write(&file, &env)?;
                    }
                    comments_to_write.extend(
                        doc.videos
                            .iter()
                            .filter(|v| v.owner_mid != 0)
                            .cloned()
                            .map(|video| (uid, video)),
                    );
                }
                if reuse_comment_view {
                    comment_files.entry(uid).or_default().push(doc);
                } else {
                    comment_files.clear();
                }
            }
            count += 1;
        }
    }
    anyhow::ensure!(count > 0, "没有找到可读取的新版归档");
    resource_copies.finish()?;
    for (uid, video) in comments_to_write {
        export::export_item_for(&target, &video, uid)?;
    }
    for (uid, path) in reports {
        anyhow::ensure!(accounts.contains_key(&uid), "采集报告与导入账号不匹配");
        let dir = archive_target.join(uid.to_string()).join("reports");
        if new_library
            && staged_archive
                .as_ref()
                .is_some_and(|root| path.starts_with(root))
        {
            let dest = dir.join(path.file_name().context("采集报告缺少文件名")?);
            if !dest.exists() {
                move_import_file(&path, &dest)?;
                continue;
            }
        }
        save_imported_report(&dir, &path)?;
    }
    for detail in my_comments {
        let uid = detail.account_uid.parse::<u64>()?;
        anyhow::ensure!(accounts.contains_key(&uid), "资料包中的我的评论没有对应账号");
        let root = messages::base(&target, uid);
        if crate::my_comments::read(&root, uid, &detail.rpid).is_none() {
            crate::my_comments::write(&root, &detail)?;
        }
        count += 1;
    }
    for stamp in collection_times {
        anyhow::ensure!(accounts.contains_key(&stamp.uid), "采集时间与导入账号不匹配");
        stamp.merge(&archive_target)?;
    }
    let first = accounts.values().next().context("资料包没有账号")?;
    let mut first_messages = None;
    for (uid, docs) in message_files {
        let mut store = if new_library {
            messages::Store {
                doc: messages::Doc::new(uid),
                root: messages::base(&target, uid),
                notice: String::new(),
            }
        } else {
            messages::load_for_import(&target, uid, workers)?
        };
        store.doc.merge_many(docs)?;
        if let Some(groups) = message_groups.get(&uid) {
            messages::save_groups(&store, groups)?;
        }
        if first.uid == uid {
            first_messages = Some(store);
        }
    }
    let selected_comments = if reuse_comment_view {
        combine_comments(
            &destination,
            first.uid,
            comment_files.remove(&first.uid).unwrap_or_default(),
        )
    } else {
        let mut doc = read_comments(&target, first.uid, workers)?;
        doc.archive["resource_root"] = json!(archive::root(&destination));
        doc
    };
    let mut selected_messages = match first_messages {
        Some(store) => store,
        None => load_account_messages(&target, first.uid, workers)?,
    };
    if first.uid != 0 {
        selected_messages.root = messages::base(&destination, first.uid);
    }
    let mut incoming = vec![];
    let mut conflicts = 0;
    if let Some(source) = state_source {
        for group in ["checkpoints", "ng"] {
            for file in safe_files(&source.join(group))? {
                let rel = file
                    .strip_prefix(&source)?
                    .to_string_lossy()
                    .replace('\\', "/");
                let Some(uid) = accounts.keys().copied().find(|u| state_allowed(&rel, *u)) else {
                    bail!("资料包状态文件归属不明");
                };
                let value: Value = serde_json::from_slice(&std::fs::read(&file)?)?;
                anyhow::ensure!(
                    value["uid"].as_u64() == Some(uid) && value.get("archive_root").is_none(),
                    "资料包状态包含旧路径或账号不匹配"
                );
                if let Some(id) = value["library_id"].as_str() {
                    anyhow::ensure!(id == manifest.library_id, "资料包状态资料库不匹配");
                }
                validate_state(&rel, &value, uid, &manifest.library_id)?;
                let suffix = rel.strip_prefix(&format!("{group}/")).unwrap();
                let dest = checkpoint::data_dir()?
                    .join(group)
                    .join(&manifest.library_id)
                    .join(suffix);
                if dest.exists() {
                    if std::fs::read(&dest)? != std::fs::read(&file)? {
                        conflicts += 1;
                    }
                } else {
                    incoming.push((file, dest));
                }
            }
        }
    }
    if conflicts > 0 {
        let answer=rfd::MessageDialog::new().set_title("保留本机已有进度").set_description(format!("发现 {conflicts} 项断点或 NG 冲突。本机已有记录将保留，只合并归档内容；取消则不导入。" )).set_buttons(rfd::MessageButtons::OkCancel).show();
        if answer != rfd::MessageDialogResult::Ok {
            bail!("已取消导入，原资料未改动");
        }
    }
    let mut c = catalog()?;
    for account in accounts.values() {
        if let Some(old) = c.accounts.iter_mut().find(|a| a.key() == account.key()) {
            if old.name.starts_with("用户 ") {
                old.name = account.name.clone();
            }
            if old.avatar.is_empty() {
                old.avatar = account.avatar.clone();
            }
            if old.avatar_id.is_empty() {
                old.avatar_id = account.avatar_id.clone();
            }
            if old.avatar_url.is_empty() {
                old.avatar_url = account.avatar_url.clone();
            }
        } else {
            c.accounts.push(account.clone());
        }
    }
    c.active = Some(first.key());
    set_foot(ui, "导入资料包：正在保存资料库…".into());
    let backup = stage.path().join("previous");
    if destination.exists() {
        std::fs::rename(&destination, &backup)?;
    }
    let mut installed_directory = false;
    let mut installed_state = Vec::new();
    let committed = (|| -> Result<()> {
        std::fs::rename(&target, &destination)?;
        installed_directory = true;
        for (source, dest) in incoming {
            let parent = dest.parent().context("状态文件缺少目录")?;
            std::fs::create_dir_all(parent)?;
            // Never overwrite state created after conflict detection.
            if std::fs::hard_link(&source, &dest).is_err() {
                let mut file = tempfile::NamedTempFile::new_in(parent)?;
                std::io::copy(&mut std::fs::File::open(source)?, &mut file)?;
                file.as_file().sync_all()?;
                file.persist_noclobber(&dest).map_err(|error| error.error)?;
            }
            installed_state.push(dest);
        }
        save_catalog(&c)
    })();
    if let Err(error) = committed {
        for path in installed_state {
            if let Err(cleanup) = std::fs::remove_file(&path) {
                push_log(
                    shared,
                    ui,
                    format!("无法清理未提交的状态 {}：{cleanup}", path.display()),
                );
            }
        }
        let restore = (|| -> Result<()> {
            if installed_directory {
                std::fs::rename(&destination, &target)?;
            }
            if backup.exists() {
                std::fs::rename(&backup, &destination)?;
            }
            Ok(())
        })();
        if let Err(restore) = restore {
            // Keep the old library in a recoverable location even if Windows holds files open.
            let recovery = stage.keep();
            return Err(error.context(format!(
                "恢复原资料库失败：{restore}。保留的目录：{}",
                recovery.display()
            )));
        }
        return Err(error);
    }
    publish_catalog(ui)?;
    activate_loaded(
        first,
        shared,
        ui,
        destination,
        selected_comments,
        selected_messages,
    )?;
    log_foot(
        shared,
        ui,
        format!(
            "已导入 {count} 份归档 · 耗时 {:.2} 秒 · 本机保留进度冲突 {conflicts} 项",
            started.elapsed().as_secs_f64()
        ),
    );
    Ok(())
}
fn move_import_file(source: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest.parent().context("导入文件缺少目录")?)?;
    anyhow::ensure!(!dest.exists(), "资料包含重复的归档目标");
    std::fs::rename(source, dest)?;
    Ok(())
}

fn collection_report_uid(value: &Value) -> Result<u64> {
    anyhow::ensure!(
        value["format"] == "bili-collection-report" && value["version"] == 1,
        "不是受支持的采集报告"
    );
    value["account_uid"]
        .as_str()
        .and_then(|uid| uid.parse::<u64>().ok())
        .filter(|uid| *uid != 0)
        .context("采集报告账号无效")
}

fn save_imported_report(dir: &Path, source: &Path) -> Result<()> {
    let bytes = std::fs::read(source)?;
    let mut dest = dir.join(source.file_name().context("采集报告缺少文件名")?);
    std::fs::create_dir_all(dir)?;
    if dest.exists() {
        if std::fs::read(&dest)? == bytes {
            return Ok(());
        }
        // Preserve both snapshots if the same run was exported at different stages.
        let digest = archive::hash(&String::from_utf8_lossy(&bytes));
        let stem = source.file_stem().context("采集报告缺少文件名")?;
        dest = dir.join(format!("{}-{digest}.json", stem.to_string_lossy()));
        if dest.exists() {
            anyhow::ensure!(std::fs::read(&dest)? == bytes, "采集报告副本冲突");
            return Ok(());
        }
    }
    std::fs::write(dest, bytes)?;
    Ok(())
}

fn account_bundle_files(account_root: &Path) -> Result<Vec<PathBuf>> {
    let comments = account_root.join("comments");
    let messages = account_root.join("messages");
    let reports = account_root.join("reports");
    scan_files(account_root, |path| {
        path.starts_with(&comments)
            || path.starts_with(&messages)
            || path == account_root.join(crate::collection_time::FILE_NAME)
            || path == reports
            || path.parent() == Some(reports.as_path())
    })
}

fn export_bundle(dest: &Path, shared: &Arc<Shared>) -> Result<()> {
    let uid = shared.account_uid.load(Ordering::SeqCst);
    let root = PathBuf::from(export_dir_of(&shared.cfg.lock().unwrap()));
    let manifest = library::ensure(&root)?;
    let stage = tempfile::tempdir()?;
    let marker = stage.path().join("archive-v1/.library.json");
    secure::write_config(&marker, &serde_json::to_value(&manifest)?)?;
    let mut files = vec![("archive-v1/.library.json".into(), marker)];
    let scope = if uid == 0 {
        "public".into()
    } else {
        uid.to_string()
    };
    let mut resources = BTreeSet::new();
    let archive_root = archive::root(&root);
    let account_root = archive_root.join(&scope);
    let report_dir = account_root.join("reports");
    let my_comment_dir = account_root.join("messages/my-comments");
    let mut archives = 0;
    for file in account_bundle_files(&account_root)? {
        if file.extension().is_none_or(|s| s != "json") {
            continue;
        }
        let bytes = std::fs::read(&file)?;
        if file == crate::collection_time::path(&archive_root, uid) {
            let stamp = crate::collection_time::Stamp::parse(serde_json::from_slice(&bytes)?)?;
            anyhow::ensure!(stamp.uid == uid, "导出的采集时间与账号不匹配");
        } else if file.starts_with(&report_dir) {
            if file.parent() != Some(report_dir.as_path()) {
                continue;
            }
            let report = serde_json::from_slice(&bytes)
                .with_context(|| format!("采集报告损坏：{}", file.display()))?;
            anyhow::ensure!(collection_report_uid(&report)? == uid, "采集报告账号不匹配");
        } else if file.parent() == Some(my_comment_dir.as_path()) {
            let detail: crate::my_comments::Detail = serde_json::from_slice(&bytes)?;
            detail.validate()?;
            anyhow::ensure!(detail.account_uid == uid.to_string() && file == crate::my_comments::path(&account_root.join("messages"), &detail.rpid),
                "导出的我的评论归档归属不匹配");
            archives += 1;
        } else {
            let env = archive::Envelope::parse(&bytes)
                .with_context(|| format!("账号归档损坏，资料包未导出：{}", file.display()))?;
            anyhow::ensure!(
                env.meta["account_uid"]
                    .as_str()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0)
                    == uid,
                "导出账号不匹配"
            );
            for resource in env.resources.values() {
                if let Some(path) = resource["path"].as_str().filter(|s| !s.is_empty()) {
                    if archive::resource_file(&archive_root, path).is_some() {
                        resources.insert(path.to_owned());
                    }
                }
            }
            archives += 1;
        }
        let relative = file
            .strip_prefix(&archive_root)?
            .to_string_lossy()
            .replace('\\', "/");
        files.push((format!("archive-v1/{relative}"), file));
    }
    anyhow::ensure!(archives > 0, "当前账号没有可导出的归档");
    for rel in resources {
        files.push((format!("archive-v1/{rel}"), archive::root(&root).join(rel)));
    }
    for group in ["checkpoints", "ng"] {
        let base = checkpoint::data_dir()?
            .join(group)
            .join(&manifest.library_id);
        for file in safe_files(&base)? {
            let rel = format!(
                "{group}/{}",
                file.strip_prefix(&base)?
                    .to_string_lossy()
                    .replace('\\', "/")
            );
            if !state_allowed(&rel, uid) {
                continue;
            }
            let mut value: Value = serde_json::from_slice(&std::fs::read(&file)?)?;
            anyhow::ensure!(value["uid"].as_u64() == Some(uid), "任务记录账号不匹配");
            value
                .as_object_mut()
                .context("任务记录无效")?
                .remove("archive_root");
            if group == "ng" || rel.contains("/messages/") {
                value["library_id"] = json!(manifest.library_id);
            }
            let out = stage.path().join(&rel);
            secure::write_config(&out, &value)?;
            files.push((rel, out));
        }
    }
    portable_zip::create(dest, &files)
}
pub fn run(action: Action, shared: &Arc<Shared>, ui: &Weak<AppWindow>) {
    set_busy(shared, ui, true);
    let result = (|| -> Result<()> {
        match action {
            Action::Import(paths) => import(paths, shared, ui),
            Action::Select(key) => {
                let c = catalog()?;
                let account = c
                    .accounts
                    .iter()
                    .find(|a| a.key() == key)
                    .context("离线账号不存在")?;
                activate(account, shared, ui)
            }
            Action::Restore => {
                let c = catalog()?;
                if let Some(key) = c.active {
                    let account = c
                        .accounts
                        .iter()
                        .find(|a| a.key() == key)
                        .context("上次离线账号不存在")?;
                    activate(account, shared, ui)?;
                }
                publish_catalog(ui)
            }
            Action::Export(path) => {
                export_bundle(&path, shared)?;
                log_foot(shared, ui, format!("已导出离线资料包：{}", path.display()));
                Ok(())
            }
        }
    })();
    if let Err(e) = result {
        push_log(shared, ui, format!("离线资料：{e:#}"));
        set_foot(ui, format!("离线资料处理失败：{e:#}"));
    }
    set_busy(shared, ui, false);
}
pub fn install(
    ui: &AppWindow,
    shared: &Arc<Shared>,
    tx: &std::sync::mpsc::Sender<Cmd>,
    weak: &Weak<AppWindow>,
) {
    let _ = publish_catalog(weak);
    {
        let shared = shared.clone();
        let tx = tx.clone();
        ui.on_offline_import_file(move || {
            if shared.busy.load(Ordering::SeqCst) {
                return;
            }
            if let Some(paths) = rfd::FileDialog::new()
                .set_title("导入离线资料包或新版 JSON")
                .set_directory(export_dir_of(&shared.cfg.lock().unwrap()))
                .add_filter("离线资料", &["zip", "json"])
                .pick_files()
            {
                let _ = tx.send(Cmd::Offline(Action::Import(paths)));
            }
        });
    }
    {
        let shared = shared.clone();
        let tx = tx.clone();
        ui.on_offline_import_folder(move || {
            if shared.busy.load(Ordering::SeqCst) {
                return;
            }
            if let Some(path) = rfd::FileDialog::new()
                .set_title("选择账号归档或资料库目录")
                .set_directory(export_dir_of(&shared.cfg.lock().unwrap()))
                .pick_folder()
            {
                let _ = tx.send(Cmd::Offline(Action::Import(vec![path])));
            }
        });
    }
    {
        let shared = shared.clone();
        let tx = tx.clone();
        ui.on_offline_choose(move |key| {
            if !shared.busy.load(Ordering::SeqCst) {
                let _ = tx.send(Cmd::Offline(Action::Select(key.to_string())));
            }
        });
    }
    {
        let shared = shared.clone();
        let tx = tx.clone();
        ui.on_offline_export(move || {
            if shared.busy.load(Ordering::SeqCst) {
                return;
            }
            let uid = shared.account_uid.load(Ordering::SeqCst);
            if let Some(path) = rfd::FileDialog::new()
                .set_title("导出当前账号离线资料包")
                .set_directory(export_dir_of(&shared.cfg.lock().unwrap()))
                .set_file_name(format!("bili-library-{uid}.zip"))
                .add_filter("离线资料包", &["zip"])
                .save_file()
            {
                let _ = tx.send(Cmd::Offline(Action::Export(path)));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profile_uid_and_resources_come_from_archive_metadata_not_cookie() {
        let t = tempfile::tempdir().unwrap();
        let file =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/examples/archive-v1/likes.json");
        let env = archive::Envelope::parse(&std::fs::read(file).unwrap()).unwrap();
        let id = library::ensure(t.path()).unwrap().library_id;
        let p = profile(&env, &id);
        assert_eq!(p.uid, 12);
        assert_eq!(p.library_id, id);
        assert!(!p.name.is_empty());
    }
    #[test]
    fn catalog_roundtrip_preserves_selection_for_restart() {
        let a = Account {
            library_id: "a".repeat(32),
            uid: 12,
            name: "offline user".into(),
            avatar: String::new(),
            avatar_id: String::new(),
            avatar_url: String::new(),
        };
        let c = Catalog {
            version: 1,
            active: Some(a.key()),
            accounts: vec![a],
        };
        let decoded: Catalog = serde_json::from_slice(&serde_json::to_vec(&c).unwrap()).unwrap();
        assert_eq!(decoded.active, Some(format!("{}:12", "a".repeat(32))));
        assert_eq!(decoded.accounts.len(), 1);
    }
    #[test]
    fn bundle_preserves_collection_reports_and_import_keeps_both_snapshots() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let likes = root.join("messages/likes.json");
        let comments = root.join("comments/12/videos/1.json");
        let report = root.join("reports/run.json");
        let stamp = root.join(crate::collection_time::FILE_NAME);
        for path in [&likes, &comments, &report, &stamp] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"{}").unwrap();
        }
        let data = json!({"format":"bili-collection-report","version":1,"account_uid":"12","status":"running"});
        std::fs::write(&report, data.to_string()).unwrap();
        assert_eq!(
            account_bundle_files(root).unwrap(),
            vec![stamp, comments, likes, report.clone()]
        );
        assert_eq!(collection_report_uid(&data).unwrap(), 12);
        let restored = root.join("restored");
        save_imported_report(&restored, &report).unwrap();
        save_imported_report(&restored, &report).unwrap();
        assert_eq!(safe_files(&restored).unwrap().len(), 1);
        let mut updated = data.clone();
        updated["status"] = json!("completed");
        std::fs::write(&report, updated.to_string()).unwrap();
        save_imported_report(&restored, &report).unwrap();
        assert_eq!(safe_files(&restored).unwrap().len(), 2);
        assert_eq!(
            std::fs::read_to_string(restored.join("run.json")).unwrap(),
            data.to_string()
        );
    }

    #[test]
    fn state_import_accepts_only_the_selected_accounts_progress() {
        for name in [
            "checkpoints/messages/v2/12.json",
            "checkpoints/messages/v2/12.ng.json",
            "ng/messages/v1/12.paging.json",
            "checkpoints/12/task.json",
        ] {
            assert!(state_allowed(name, 12));
            assert!(!state_allowed(name, 13));
        }
        for name in [
            "config.json",
            "credentials.json",
            "ng/messages/v1/13.paging.json",
            "checkpoints/messages/v2/13.json",
        ] {
            assert!(!state_allowed(name, 12));
        }
    }
}
