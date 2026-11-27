//! Durable, account/root-isolated paging failures; content archives are unchanged.
use super::*;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum Target {
    Feed(String),
    List(u32),
    Session(String),
    Like(String),
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) enum Position {
    Feed(String, Cursor),
    List(u32, Cursor),
    Session(String, SessionTask),
    Like(String, likes::LikeCursor),
}
impl Position {
    pub fn target(&self) -> Target {
        match self {
            Self::Feed(t, _) => Target::Feed(t.clone()),
            Self::List(c, _) => Target::List(*c),
            Self::Session(k, _) => Target::Session(k.clone()),
            Self::Like(k, _) => Target::Like(k.clone()),
        }
    }
    fn key(&self) -> Result<String> {
        let cursor = match self {
            Self::Feed(_, c) => json!([c.id, c.time]),
            Self::List(_, c) => json!(c.time),
            Self::Session(_, s) => json!(s.seq),
            Self::Like(_, c) => c.request_cursor(),
        };
        Ok(digest(&serde_json::to_string(&(self.target(), cursor))?))
    }
    pub fn apply(&self, task: &mut Task) {
        match self {
            Self::Feed(t, c) => {
                task.feeds.insert(t.clone(), c.clone());
            }
            Self::List(t, c) => {
                task.lists.insert(*t, c.clone());
            }
            Self::Session(t, c) => {
                task.sessions.insert(t.clone(), c.clone());
            }
            Self::Like(t, c) => {
                task.likes.insert(t.clone(), c.clone());
            }
        }
    }
    pub fn name(&self) -> String {
        match self {
            Self::Feed(t, _) => label(t).into(),
            Self::List(c, _) => format!("私信会话分类 {}", c),
            Self::Session(k, _) => format!("私信 {}", k),
            Self::Like(k, _) => format!("点赞通知 {}", k),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Job {
    pub position: Position,
    pub options: Options,
    pub status: String,
    pub reason: String,
    pub retries: u32,
    pub diagnostic: Value,
    #[serde(default)]
    continuation: bool,
}
#[derive(Default, Serialize, Deserialize)]
struct Registry {
    version: u32,
    uid: u64,
    library_id: String,
    jobs: BTreeMap<String, Job>,
}
#[derive(Clone, Copy, Default)]
pub struct PagingStats {
    pub deferred: usize,
    pub recovered: usize,
    pub ng: usize,
    pub skipped_ng: usize,
}
pub(super) struct Paging {
    path: PathBuf,
    registry: Registry,
    current: Option<(String, Job)>,
    pub only: Option<Target>,
    active: Option<String>,
    attempted: BTreeSet<String>,
    eligible: Option<BTreeSet<String>>,
    skipped: BTreeSet<String>,
    pub stats: PagingStats,
}
pub(super) fn validate_portable(value: Value, uid: u64, library_id: &str) -> Result<()> {
    let registry: Registry = serde_json::from_value(value).context("分页 NG 内容无效")?;
    anyhow::ensure!(registry.version == 1 && registry.uid == uid && registry.library_id == library_id, "分页 NG 归属或版本不匹配");
    Ok(())
}
fn registry_name(task_path: &Path, _root: &str) -> String {
    let file = task_path.file_name().unwrap_or_default().to_string_lossy();
    format!(
        "{}.paging.json",
        file.strip_suffix(".ng.json")
            .or_else(|| file.strip_suffix(".json"))
            .unwrap_or(&file)
    )
}
fn registry_path(task_path: &Path, root: &str) -> Result<PathBuf> {
    let parent = task_path.parent().unwrap_or(Path::new("."));
    let data = parent
        .ancestors()
        .find(|p| p.file_name().is_some_and(|n| n == "checkpoints"))
        .and_then(Path::parent)
        .unwrap_or(parent);
    let id = crate::library::id_for_archive(Path::new(root))?;
    Ok(data
        .join("ng")
        .join(id)
        .join("messages/v1")
        .join(registry_name(task_path, root)))
}
fn load_registry(task_path: &Path, uid: u64, root: &str) -> Result<Registry> {
    let path = registry_path(task_path, root)?;
    let library_id = crate::library::id_for_archive(Path::new(root))?;
    let read = |path: &Path| -> Result<Registry> {
        let value = crate::library::read_state(path, uid, Path::new(root))?
            .context("分页异常记录资料库不匹配")?;
        let registry: Registry =
            serde_json::from_value(value).context("分页异常记录损坏，未覆盖")?;
        anyhow::ensure!(
            registry.version == 1 && registry.uid == uid && registry.library_id == library_id,
            "分页异常记录账号或资料库不匹配"
        );
        Ok(registry)
    };
    let mut registry = if path.exists() {
        read(&path)?
    } else {
        Registry {
            version: 1,
            uid,
            library_id: library_id.clone(),
            ..Default::default()
        }
    };
    let data = path
        .ancestors()
        .find(|p| p.file_name().is_some_and(|n| n == "ng"))
        .and_then(Path::parent)
        .unwrap_or_else(|| task_path.parent().unwrap_or(Path::new(".")));
    let oldname = format!("{}-{}.paging.json", uid, digest(root));
    for legacy in [
        task_path.with_file_name(oldname.clone()),
        data.join("ng/messages/v1").join(&oldname),
        data.join("checkpoints/messages/v2").join(&oldname),
    ] {
        if !legacy.exists() {
            continue;
        }
        let old = read(&legacy)?;
        for (key, job) in old.jobs {
            registry.jobs.entry(key).or_insert(job);
        }
        let value = serde_json::to_value(&registry)?;
        crate::secure::write_config(&path, &value).context(PersistFailure)?;
        let saved: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        anyhow::ensure!(saved == value, "NG 迁移校验失败，原文件已保留");
        crate::library::backup_remove(&legacy)?;
    }
    Ok(registry)
}
impl Paging {
    pub fn open(task_path: &Path, uid: u64, root: &str, manual: bool) -> Result<Self> {
        let path = registry_path(task_path, root)?;
        let mut registry = load_registry(task_path, uid, root)?;
        let mut eligible = BTreeSet::new();
        for (key, job) in &mut registry.jobs {
            if job.status == "retrying" {
                job.status = "pending".into();
            }
            if manual && job.status == "ng" {
                eligible.insert(key.clone());
            }
        }
        let state = Self {
            path,
            registry,
            current: None,
            only: None,
            active: None,
            attempted: BTreeSet::new(),
            eligible: manual.then_some(eligible),
            skipped: BTreeSet::new(),
            stats: PagingStats::default(),
        };
        // Mark interrupted attempts pending before any request. Cancellation never spends a retry.
        state.save()?;
        Ok(state)
    }
    fn save(&self) -> Result<()> {
        crate::secure::write_config(&self.path, &serde_json::to_value(&self.registry)?)
            .context(PersistFailure)
    }
    pub fn allows(&self, target: &Target) -> bool {
        match &self.only {
            None => true,
            Some(t) if t == target => true,
            Some(Target::Feed(t)) if t == "likes" => matches!(target, Target::Like(_)),
            Some(Target::List(_)) => matches!(target, Target::Session(_)),
            _ => false,
        }
    }
    pub fn begin(
        &mut self,
        position: Position,
        options: &Options,
    ) -> Result<(bool, Option<String>)> {
        let key = position.key()?;
        // A refreshed like summary may invalidate its old offsets. Transfer the one
        // active attempt to the reset cursor rather than leaving an orphaned retrying job.
        let displaced = self
            .active
            .as_ref()
            .and_then(|old| {
                self.registry
                    .jobs
                    .get(old)
                    .map(|j| (old.clone(), j.clone()))
            })
            .filter(|(old, j)| {
                old != &key && j.status == "retrying" && j.position.target() == position.target()
            });
        if let Some((old, mut job)) = displaced {
            self.registry.jobs.get_mut(&old).unwrap().status = "superseded".into();
            job.position = position.clone();
            job.options = options.clone();
            self.registry.jobs.insert(key.clone(), job);
            self.active = Some(key.clone());
            self.attempted.insert(key.clone());
            if let Some(eligible) = &mut self.eligible {
                eligible.insert(key.clone());
            }
            self.save()?;
        }
        if let Some(job) = self.registry.jobs.get(&key) {
            if job.status == "ng" {
                let text = if self.skipped.insert(key.clone()) {
                    self.stats.skipped_ng += 1;
                    Some(format!("跳过已有 NG · {}", position.name()))
                } else {
                    None
                };
                return Ok((false, text));
            }
            if matches!(job.status.as_str(), "pending" | "retrying")
                && self.active.as_ref() != Some(&key)
            {
                return Ok((false, None));
            }
        }
        self.current = Some((
            key,
            Job {
                position,
                options: options.clone(),
                status: "pending".into(),
                reason: String::new(),
                retries: 0,
                diagnostic: Value::Null,
                continuation: false,
            },
        ));
        Ok((true, None))
    }
    pub fn failed(&mut self, reason: String, diagnostic: Value) -> Result<(String, Job)> {
        let (key, mut job) = self.current.take().context("分页请求缺少目标")?;
        let continuation = self.registry.jobs.get(&key).is_some_and(|j| j.continuation);
        let ng = self.active.as_ref() == Some(&key) && !continuation;
        if continuation {
            self.attempted.remove(&key);
        }
        job.status = if ng { "ng" } else { "pending" }.into();
        job.reason = reason;
        job.diagnostic = diagnostic;
        job.retries = u32::from(ng);
        if ng {
            self.stats.ng += 1;
        } else {
            self.stats.deferred += 1;
        }
        if let Some(eligible) = &mut self.eligible {
            eligible.insert(key.clone());
        }
        self.registry.jobs.insert(key.clone(), job.clone());
        self.save()?;
        Ok((key, job))
    }
    pub fn accepted(&mut self) -> Result<Option<(String, Job)>> {
        let Some((key, _)) = self.current.take() else {
            return Ok(None);
        };
        if self.active.as_ref() != Some(&key) {
            return Ok(None);
        }
        if let Some(job) = self.registry.jobs.get_mut(&key) {
            job.status = "recovered".into();
            job.retries = 1;
            let job = job.clone();
            if !job.continuation {
                self.stats.recovered += 1;
            }
            self.save()?;
            return Ok(if job.continuation {
                None
            } else {
                Some((key, job))
            });
        }
        Ok(None)
    }
    pub fn next(&mut self) -> Result<Option<(String, Job)>> {
        let next = self
            .registry
            .jobs
            .iter()
            .find(|(key, j)| {
                (j.status == "pending"
                    || j.status == "ng" && self.eligible.as_ref().is_some_and(|e| e.contains(*key)))
                    && !self.attempted.contains(*key)
                    && self.eligible.as_ref().is_none_or(|e| e.contains(*key))
            })
            .map(|(k, j)| (k.clone(), j.clone()));
        if let Some((key, job)) = &next {
            self.attempted.insert(key.clone());
            self.active = Some(key.clone());
            self.only = Some(job.position.target());
            self.registry.jobs.get_mut(key).unwrap().status = "retrying".into();
            self.save()?;
        }
        Ok(next)
    }
    pub fn suspend(&mut self, task: &Task) -> Result<()> {
        let Some(job) = self
            .active
            .as_ref()
            .and_then(|k| self.registry.jobs.get(k))
            .cloned()
        else {
            return Ok(());
        };
        if job.status != "recovered" {
            return Ok(());
        }
        let position = match job.position.target() {
            Target::Feed(t) => task
                .feeds
                .get(&t)
                .filter(|c| !c.done)
                .map(|c| Position::Feed(t, c.clone())),
            Target::List(t) => task
                .lists
                .get(&t)
                .filter(|c| !c.done)
                .map(|c| Position::List(t, c.clone())),
            Target::Session(t) => task
                .sessions
                .get(&t)
                .filter(|c| !c.done)
                .map(|c| Position::Session(t, c.clone())),
            Target::Like(t) => task
                .likes
                .get(&t)
                .filter(|c| !c.is_done())
                .map(|c| Position::Like(t, c.clone())),
        };
        if let Some(position) = position {
            let key = position.key()?;
            if !self
                .registry
                .jobs
                .get(&key)
                .is_some_and(|j| matches!(j.status.as_str(), "pending" | "retrying" | "ng"))
            {
                self.registry.jobs.insert(
                    key,
                    Job {
                        position,
                        options: job.options,
                        status: "pending".into(),
                        reason: "补试中断，进度已保存".into(),
                        retries: 0,
                        diagnostic: Value::Null,
                        continuation: true,
                    },
                );
                self.save()?;
            }
        }
        Ok(())
    }
    pub fn remaining(&self) -> usize {
        self.registry
            .jobs
            .iter()
            .filter(|(k, j)| {
                (matches!(j.status.as_str(), "pending" | "retrying")
                    || j.status == "ng"
                        && self.eligible.as_ref().is_some_and(|e| e.contains(*k))
                        && !self.attempted.contains(*k))
                    && self.eligible.as_ref().is_none_or(|e| e.contains(*k))
            })
            .count()
    }
    pub fn unresolved(&self) -> usize {
        self.registry
            .jobs
            .values()
            .filter(|j| matches!(j.status.as_str(), "pending" | "retrying" | "ng"))
            .count()
    }
}
// Only runnable normal work is a checkpoint. NG is handled by the explicit NG action.
pub(super) fn has_resumable_at(path: &Path, uid: u64, root: &str, doc: &Doc) -> Result<bool> {
    let registry = load_registry(path, uid, root)?;
    if registry
        .jobs
        .values()
        .any(|j| matches!(j.status.as_str(), "pending" | "retrying"))
    {
        return Ok(true);
    }
    if !path.exists() {
        return Ok(false);
    }
    let Some(value) = crate::library::read_state(path, uid, Path::new(root))? else {
        return Ok(false);
    };
    let task: Task = serde_json::from_value(value).context("消息断点损坏，未覆盖")?;
    anyhow::ensure!(
        task.version == 2 && task.uid == uid,
        "消息断点版本或账号不匹配"
    );
    if task.library_id != crate::library::id_for_archive(Path::new(root))? {
        return Ok(false);
    }
    if task.options.interaction_recollect || task.pending_my_comments {
        return Ok(true);
    }
    let blocked = |position: Position| -> Result<bool> {
        Ok(registry
            .jobs
            .get(&position.key()?)
            .is_some_and(|j| j.status == "ng"))
    };
    for ty in task
        .options
        .types
        .iter()
        .filter(|t| !["private", "groups"].contains(&t.as_str()))
    {
        let cursor = task.feeds.get(ty).cloned().unwrap_or_default();
        if !cursor.done && !blocked(Position::Feed(ty.clone(), cursor))? {
            return Ok(true);
        }
    }
    for category in task.options.session_categories() {
        let cursor = task.lists.get(&category).cloned().unwrap_or_default();
        if !cursor.done && !blocked(Position::List(category, cursor))? {
            return Ok(true);
        }
    }
    for (key, st) in &task.sessions {
        if task.options.session_enabled(&st.session)
            && !st.done
            && !blocked(Position::Session(key.clone(), st.clone()))?
        {
            return Ok(true);
        }
    }
    if task.options.types.iter().any(|t| t == "likes") {
        // A paused run may have saved the notice feed before creating detail cursors.
        for item in doc.items.iter().filter(|i| {
            i.ty == "likes"
                && (task.options.contains(i.timestamp) || task.likes.contains_key(&i.id))
        }) {
            let mut cursor = task.likes.get(&item.id).cloned().unwrap_or_default();
            if cursor.is_done() && item.like_detail.status == LikeStatus::Finished {
                continue;
            }
            if cursor.is_done() || item.like_detail.status == LikeStatus::Pending {
                cursor = likes::LikeCursor::default();
            }
            if !blocked(Position::Like(item.id.clone(), cursor))? {
                return Ok(true);
            }
        }
        // Missing source data remains an incomplete checkpoint; do not scan all
        // archived items once for every saved cursor during each UI refresh.
        let known: BTreeSet<&str> = doc
            .items
            .iter()
            .filter(|i| i.ty == "likes")
            .map(|i| i.id.as_str())
            .collect();
        if task
            .likes
            .iter()
            .any(|(key, c)| !c.is_done() && !known.contains(key.as_str()))
        {
            return Ok(true);
        }
    }
    Ok(false)
}
pub(super) fn ng_count_at(path: &Path, uid: u64, root: &str) -> Result<usize> {
    let registry = load_registry(path, uid, root)?;
    Ok(registry.jobs.values().filter(|j| j.status == "ng").count())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn options() -> Options {
        Options::parse(vec!["replies".into()], "", "").unwrap()
    }
    fn position(id: &str) -> Position {
        Position::Feed(
            "replies".into(),
            Cursor {
                id: id.into(),
                time: 100,
                done: false,
            },
        )
    }
    fn mark_ng(paging: &mut Paging, pos: Position, opts: &Options) {
        paging.begin(pos, opts).unwrap();
        paging.failed("blocked".into(), Value::Null).unwrap();
        let (_, j) = paging.next().unwrap().unwrap();
        paging.begin(j.position, &j.options).unwrap();
        paging.failed("blocked".into(), Value::Null).unwrap();
    }
    #[test]
    fn ng_only_checkpoint_is_not_resumable_but_mixed_and_pending_work_are() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("12.json");
        let opts = options();
        let mut task = Task::open(12, opts.clone(), path.clone(), "root".into()).unwrap();
        task.feeds.insert(
            "replies".into(),
            Cursor {
                id: "1".into(),
                time: 100,
                done: false,
            },
        );
        task.save().unwrap();
        let mut paging = Paging::open(&path, 12, "root", false).unwrap();
        mark_ng(&mut paging, position("1"), &opts);
        assert!(!has_resumable_at(&path, 12, "root", &Doc::new(12)).unwrap());
        task.options.types.push("mentions".into());
        task.save().unwrap();
        assert!(has_resumable_at(&path, 12, "root", &Doc::new(12)).unwrap());
        task.options = opts.clone();
        task.save().unwrap();
        let key = position("1").key().unwrap();
        paging.registry.jobs.get_mut(&key).unwrap().status = "pending".into();
        paging.save().unwrap();
        assert!(has_resumable_at(&path, 12, "root", &Doc::new(12)).unwrap());
        // Deferred queue is resumable independently of the ordinary checkpoint file.
        std::fs::remove_file(&path).unwrap();
        assert!(has_resumable_at(&path, 12, "root", &Doc::new(12)).unwrap());
    }
    #[test]
    fn ng_like_cursors_do_not_make_completed_collection_a_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("12.json");
        let opts = Options::parse(vec!["likes".into()], "", "").unwrap();
        let mut task = Task::open(12, opts.clone(), path.clone(), "root".into()).unwrap();
        task.feeds.insert(
            "likes".into(),
            Cursor {
                done: true,
                ..Default::default()
            },
        );
        task.likes.insert("7".into(), likes::LikeCursor::default());
        task.save().unwrap();
        let mut doc = Doc::new(12);
        doc.items.push(Item {
            id: "7".into(),
            ty: "likes".into(),
            timestamp: 100,
            like_detail: LikeDetail {
                status: LikeStatus::Partial,
                error: "NG".into(),
            },
            ..Default::default()
        });
        let mut paging = Paging::open(&path, 12, "root", false).unwrap();
        mark_ng(
            &mut paging,
            Position::Like("7".into(), likes::LikeCursor::default()),
            &opts,
        );
        assert!(!has_resumable_at(&path, 12, "root", &doc).unwrap());
        // Feed saved before detail collection: no detail cursor yet, but real normal work remains.
        doc.items.push(Item {
            id: "8".into(),
            ty: "likes".into(),
            timestamp: 99,
            ..Default::default()
        });
        assert!(has_resumable_at(&path, 12, "root", &doc).unwrap());
    }
    #[test]
    fn completed_tasks_and_other_export_directories_do_not_show_continue() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("12.json");
        let mut task = Task::open(12, options(), path.clone(), "root".into()).unwrap();
        task.feeds.insert(
            "replies".into(),
            Cursor {
                done: true,
                ..Default::default()
            },
        );
        task.save().unwrap();
        assert!(!has_resumable_at(&path, 12, "root", &Doc::new(12)).unwrap());
        assert!(!has_resumable_at(&path, 12, "other-root", &Doc::new(12)).unwrap());
    }
    #[test]
    fn ng_store_is_outside_checkpoint_tree() {
        let dir = tempfile::tempdir().unwrap();
        let task = dir.path().join("checkpoints/messages/v2/12.json");
        let registry = registry_path(&task, "root").unwrap();
        assert!(registry.starts_with(
            dir.path()
                .join("ng")
                .join(crate::library::id_for_archive(Path::new("root")).unwrap())
                .join("messages/v1")
        ));
        assert!(!registry.starts_with(dir.path().join("checkpoints")));
    }
    #[test]
    fn old_registry_moves_only_after_verified_write_and_preserves_newer_state() {
        let dir = tempfile::tempdir().unwrap();
        let task = dir.path().join("12.json");
        let opts = options();
        let mut paging = Paging::open(&task, 12, "root", false).unwrap();
        paging.begin(position("1"), &opts).unwrap();
        paging.failed("blocked".into(), Value::Null).unwrap();
        let (key, job) = paging.next().unwrap().unwrap();
        paging.begin(job.position, &job.options).unwrap();
        paging.failed("blocked".into(), Value::Null).unwrap();
        let path = registry_path(&task, "root").unwrap();
        let legacy = task.with_file_name(format!("12-{}.paging.json", digest("root")));
        std::fs::rename(&path, &legacy).unwrap();
        let migrated = Paging::open(&task, 12, "root", false).unwrap();
        assert!(!legacy.exists());
        assert_eq!(migrated.registry.jobs[&key].status, "ng");
        std::fs::copy(&path, &legacy).unwrap();
        let mut newer = load_registry(&task, 12, "root").unwrap();
        newer.jobs.get_mut(&key).unwrap().status = "recovered".into();
        crate::secure::write_config(&path, &serde_json::to_value(&newer).unwrap()).unwrap();
        // An old copy must not resurrect NG after recovery in the independent store.
        let mut old = Registry {
            version: 1,
            uid: 12,
            library_id: crate::library::id_for_archive(Path::new("root")).unwrap(),
            ..Default::default()
        };
        let mut old_job = newer.jobs[&key].clone();
        old_job.status = "ng".into();
        old.jobs.insert(key.clone(), old_job);
        crate::secure::write_config(&legacy, &serde_json::to_value(old).unwrap()).unwrap();
        let merged = load_registry(&task, 12, "root").unwrap();
        assert_eq!(merged.jobs[&key].status, "recovered");
        assert!(!legacy.exists());
    }
    #[test]
    fn corrupt_legacy_registry_is_retained() {
        let dir = tempfile::tempdir().unwrap();
        let task = dir.path().join("12.json");
        let legacy = task.with_file_name(format!("12-{}.paging.json", digest("root")));
        std::fs::write(&legacy, b"broken").unwrap();
        assert!(load_registry(&task, 12, "root").is_err());
        assert_eq!(std::fs::read(&legacy).unwrap(), b"broken");
        assert!(!registry_path(&task, "root").unwrap().exists());
    }
    #[test]
    fn defer_once_then_persist_ng_and_only_manual_retry_can_recover_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("12.json");
        let opts = options();
        let mut p = Paging::open(&path, 12, "root-a", false).unwrap();
        assert!(p.begin(position("1"), &opts).unwrap().0);
        p.failed(
            "same cursor".into(),
            json!({"reason":"non_advancing_cursor"}),
        )
        .unwrap();
        assert!(!p.begin(position("1"), &opts).unwrap().0);
        let (_, job) = p.next().unwrap().unwrap();
        assert_eq!(job.retries, 0);
        assert_eq!(job.options, opts);
        assert!(p.begin(job.position, &job.options).unwrap().0);
        p.failed(
            "still blocked".into(),
            json!({"reason":"non_advancing_cursor"}),
        )
        .unwrap();
        assert!(p.next().unwrap().is_none());
        assert_eq!(p.stats.ng, 1);
        let mut p = Paging::open(&path, 12, "root-a", false).unwrap();
        assert!(p.next().unwrap().is_none());
        assert!(!p.begin(position("1"), &opts).unwrap().0);
        assert_eq!(p.stats.skipped_ng, 1);
        let mut p = Paging::open(&path.with_extension("ng.json"), 12, "root-a", true).unwrap();
        let (_, job) = p.next().unwrap().unwrap();
        assert!(p.begin(job.position, &job.options).unwrap().0);
        assert!(p.accepted().unwrap().is_some());
        assert_eq!(p.stats.recovered, 1);
        assert_eq!(p.unresolved(), 0);
    }
    #[test]
    fn interrupted_request_remains_pending_and_does_not_spend_failure_budget() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("12.json");
        let opts = options();
        let mut p = Paging::open(&path, 12, "root", false).unwrap();
        p.begin(position("1"), &opts).unwrap();
        p.failed("blocked".into(), Value::Null).unwrap();
        p.next().unwrap();
        drop(p);
        let mut p = Paging::open(&path, 12, "root", false).unwrap();
        let (_, job) = p.next().unwrap().unwrap();
        assert_eq!(job.retries, 0);
        p.begin(job.position, &job.options).unwrap();
        p.accepted().unwrap();
        assert_eq!(p.stats.ng, 0);
    }
    #[test]
    fn recovered_target_continuation_is_durable_after_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("12.json");
        let opts = options();
        let mut p = Paging::open(&path, 12, "root", false).unwrap();
        p.begin(position("1"), &opts).unwrap();
        p.failed("blocked".into(), Value::Null).unwrap();
        let (_, job) = p.next().unwrap().unwrap();
        p.begin(job.position, &job.options).unwrap();
        p.accepted().unwrap();
        let mut task = Task::open(12, opts.clone(), path.clone(), "root".into()).unwrap();
        task.feeds.insert(
            "replies".into(),
            Cursor {
                id: "2".into(),
                time: 90,
                done: false,
            },
        );
        p.suspend(&task).unwrap();
        drop(p);
        let mut p = Paging::open(&path, 12, "root", false).unwrap();
        let (_, job) = p.next().unwrap().unwrap();
        assert!(job.continuation);
        p.begin(job.position.clone(), &job.options).unwrap();
        // The resumed request is not itself a consumed anomaly retry.
        p.failed("new stall".into(), Value::Null).unwrap();
        assert_eq!(p.stats.ng, 0);
        let (_, job) = p.next().unwrap().unwrap();
        p.begin(job.position, &job.options).unwrap();
        p.failed("still stalled".into(), Value::Null).unwrap();
        assert_eq!(p.stats.ng, 1);
    }
    #[test]
    fn roots_accounts_and_request_cursors_are_isolated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("12.json");
        let opts = options();
        let mut p = Paging::open(&path, 12, "root-a", false).unwrap();
        p.begin(position("1"), &opts).unwrap();
        p.failed("blocked".into(), Value::Null).unwrap();
        assert!(p.begin(position("2"), &opts).unwrap().0);
        assert_eq!(
            Paging::open(&path, 12, "root-b", false)
                .unwrap()
                .unresolved(),
            0
        );
        assert_eq!(
            Paging::open(&dir.path().join("13.json"), 13, "root-a", false)
                .unwrap()
                .unresolved(),
            0
        );
        assert!(Paging::open(&path, 13, "root-a", false).is_err());
    }
    #[test]
    fn ordinary_task_cleanup_does_not_remove_ng_registry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("12.json");
        let opts = options();
        std::fs::write(&path, b"temporary task").unwrap();
        let mut p = Paging::open(&path, 12, "root", false).unwrap();
        p.begin(position("1"), &opts).unwrap();
        p.failed("blocked".into(), Value::Null).unwrap();
        let (_, j) = p.next().unwrap().unwrap();
        p.begin(j.position, &j.options).unwrap();
        p.failed("blocked".into(), Value::Null).unwrap();
        std::fs::remove_file(&path).unwrap();
        let mut p = Paging::open(&path, 12, "root", false).unwrap();
        assert_eq!(p.unresolved(), 1);
        assert!(!p.begin(position("1"), &opts).unwrap().0);
    }
    #[test]
    fn broken_registry_is_never_silently_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("12.json");
        Paging::open(&path, 12, "root", false).unwrap();
        let registry = registry_path(&path, "root").unwrap();
        std::fs::write(&registry, b"broken").unwrap();
        assert!(Paging::open(&path, 12, "root", false).is_err());
        assert_eq!(std::fs::read(&registry).unwrap(), b"broken");
    }
}
