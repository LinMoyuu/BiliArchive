//! Task events and reports are separate from archival content.
use super::*;
use std::time::Duration;

#[derive(Clone, Debug)]
pub enum CrawlEvent {
    Stage(String),
    Progress(String),
    Waiting {
        reason: String,
        detail: String,
        seconds: u64,
    },
    Tick(u64),
    Retrying(String),
    Recovered(String),
    Deferred(String),
    Issue(String),
    Report(PathBuf),
}
pub struct RunOutcome {
    pub paging: paging::PagingStats,
    pub paging_unresolved: usize,
    pub errors: Vec<String>,
    pub report: PathBuf,
}
pub(super) struct CrawlContext<'a> {
    pub live: bool,
    pub(super) paging: Option<paging::Paging>,
    pub(super) manual_paging: bool,
    emit: &'a mut dyn FnMut(CrawlEvent),
    sleep: &'a mut dyn FnMut(Duration),
    cancel: &'a AtomicBool,
    report: Value,
    path: Option<PathBuf>,
    store: Option<Arc<Mutex<Store>>>,
    stage: String,
    target: String,
    waiting: Option<usize>,
    pending_issues: Vec<usize>,
}
impl<'a> CrawlContext<'a> {
    pub fn new(
        cancel: &'a AtomicBool,
        emit: &'a mut dyn FnMut(CrawlEvent),
        sleep: &'a mut dyn FnMut(Duration),
    ) -> Self {
        Self {
            live: false,
            paging: None,
            manual_paging: false,
            emit,
            sleep,
            cancel,
            report: Value::Null,
            path: None,
            store: None,
            stage: String::new(),
            target: String::new(),
            waiting: None,
            pending_issues: Vec::new(),
        }
    }
    pub fn start(&mut self, root: &Path, uid: u64, options: &Options) -> Result<()> {
        self.live = true;
        let run_id = format!(
            "{}-{}",
            chrono::Local::now().format("%Y%m%dT%H%M%S%9f"),
            std::process::id()
        );
        let dir = crate::archive::storage_root(root)
            .join(uid.to_string())
            .join("reports");
        std::fs::create_dir_all(&dir).context(PersistFailure)?;
        if !dir
            .canonicalize()?
            .starts_with(crate::archive::storage_root(root).canonicalize()?)
        {
            bail!("采集报告目录越界");
        }
        self.path = Some(dir.join(format!("{run_id}.json")));
        self.report = json!({"format":"bili-collection-report","version":1,"run_id":run_id,"account_uid":uid.to_string(),"started_at":chrono::Local::now().to_rfc3339(),"ended_at":null,"scope":options,"status":"running","archived_total":0,"counts":{},"issues":[]});
        self.save()?;
        (self.emit)(CrawlEvent::Report(self.path.clone().unwrap()));
        Ok(())
    }
    pub fn monitor(&mut self, store: &Arc<Mutex<Store>>) {
        self.store = Some(Arc::clone(store));
    }
    fn save(&mut self) -> Result<()> {
        if let Some(store) = &self.store {
            let state = store.lock().unwrap();
            self.report["archived_total"] = json!(state.doc.items.len());
            let counts: BTreeMap<_, _> = VIEW_TYPES
                .iter()
                .map(|t| {
                    (
                        *t,
                        state
                            .doc
                            .items
                            .iter()
                            .filter(|i| item_type(i) == *t)
                            .count(),
                    )
                })
                .collect();
            self.report["counts"] = json!(counts);
        }
        if let Some(path) = &self.path {
            crate::secure::write_config(path, &self.report).context(PersistFailure)?;
        }
        Ok(())
    }
    pub fn progress(&mut self, text: String, target: &str) {
        if self.target != target {
            (self.emit)(CrawlEvent::Stage(text.clone()));
        }
        self.stage = text.clone();
        self.target = target.into();
        (self.emit)(CrawlEvent::Progress(text));
    }
    pub fn update(&mut self, text: String) {
        (self.emit)(CrawlEvent::Progress(text));
    }
    pub fn issue(&mut self, detail: String) -> Result<()> {
        let stage = self.stage.clone();
        let target = self.target.clone();
        self.issue_at("fatal", &stage, &target, detail)
    }
    pub fn resource_issue(&mut self, key: &str, detail: String) -> Result<()> {
        self.issue_at("resource", "附件处理", key, detail)
    }
    fn issue_at(
        &mut self,
        issue_type: &str,
        stage: &str,
        target: &str,
        detail: String,
    ) -> Result<()> {
        if self.live {
            self.report["issues"].as_array_mut().unwrap().push(json!({"stage":stage,"target_id":target,"time":chrono::Local::now().to_rfc3339(),"type":issue_type,"reason":detail,"retries":0,"resolved":false}));
            self.save()?;
        }
        (self.emit)(CrawlEvent::Issue(detail));
        Ok(())
    }
    pub fn wait(&mut self, reason: crate::bili::RetryReason, detail: String) -> Result<()> {
        anyhow::ensure!(
            reason != crate::bili::RetryReason::Paging,
            "分页异常必须交给延后队列"
        );
        self.wait_details(reason, detail, None)
    }

    fn wait_details(
        &mut self,
        reason: crate::bili::RetryReason,
        detail: String,
        pagination: Option<Value>,
    ) -> Result<()> {
        if !self.live {
            bail!("{}", detail);
        }
        if self.cancel.load(Ordering::SeqCst) {
            bail!("已取消");
        }
        let name = reason.label().to_owned();
        let seconds = reason.wait_seconds();
        let position = if let Some(position) = self
            .waiting
            .filter(|p| self.report["issues"][*p]["type"] == reason.key())
        {
            position
        } else {
            let list = self.report["issues"].as_array_mut().unwrap();
            list.push(json!({"stage":self.stage,"target_id":self.target,"time":chrono::Local::now().to_rfc3339(),"type":reason.key(),"reason":detail,"retries":0,"resolved":false}));
            list.len() - 1
        };
        self.waiting = Some(position);
        if !self.pending_issues.contains(&position) {
            self.pending_issues.push(position);
        }
        self.report["issues"][position]["retries"] = json!(
            self.report["issues"][position]["retries"]
                .as_u64()
                .unwrap_or(0)
                + 1
        );
        self.report["issues"][position]["reason"] = json!(detail);
        if let Some(pagination) = pagination {
            self.report["issues"][position]["pagination"] = pagination;
        }

        self.report["status"] = json!("waiting");
        self.save()?;
        (self.emit)(CrawlEvent::Waiting {
            reason: name,
            detail: format!("{}：{}", self.stage, detail),
            seconds,
        });
        let emit = &mut self.emit;
        crate::bili::wait_for_retry_seconds(
            seconds,
            self.cancel,
            &mut |n| emit(CrawlEvent::Tick(n)),
            self.sleep,
        )?;
        self.report["status"] = json!("retrying");
        self.save()?;
        (self.emit)(CrawlEvent::Retrying(self.stage.clone()));
        Ok(())
    }
    pub fn request(
        &mut self,
        request: &mut impl FnMut(&str) -> Result<Value>,
        url: &str,
    ) -> Result<Value> {
        loop {
            if self.cancel.load(Ordering::SeqCst) {
                bail!("已取消");
            }
            let result = request(url).and_then(|v| {
                data(&v)?;
                Ok(v)
            });
            match result {
                Ok(value) => return Ok(value),
                Err(error) => match crate::bili::retry_reason(&error) {
                    Some(reason) if self.live && !error.is::<PersistFailure>() => {
                        self.wait(reason, format!("{error:#}"))?
                    }
                    _ => return Err(error),
                },
            }
        }
    }
    pub fn recovered(&mut self) -> Result<()> {
        if self.waiting.take().is_some() {
            for position in self.pending_issues.drain(..) {
                self.report["issues"][position]["resolved"] = json!(true);
                self.report["issues"][position]["resolved_at"] =
                    json!(chrono::Local::now().to_rfc3339());
            }
            self.report["status"] = json!("running");
            self.save()?;
            (self.emit)(CrawlEvent::Recovered(self.stage.clone()));
        }
        Ok(())
    }
    pub fn paging_allows(&self, target: &paging::Target) -> bool {
        self.paging.as_ref().is_none_or(|p| p.allows(target))
    }
    pub fn paging_begin(&mut self, position: paging::Position, options: &Options) -> Result<bool> {
        let Some(paging) = &mut self.paging else {
            return Ok(true);
        };
        let (allowed, text) = paging.begin(position, options)?;
        if let Some(text) = text {
            (self.emit)(CrawlEvent::Deferred(text));
        }
        Ok(allowed)
    }
    pub fn paging_failed(&mut self, detail: String, diagnostic: Value) -> Result<String> {
        if self.cancel.load(Ordering::SeqCst) {
            bail!("已取消");
        }
        // Non-live callers use the same scheduler; legacy direct workers still return an error.
        let Some(paging) = &mut self.paging else {
            bail!("{}", detail)
        };
        let (key, job) = paging.failed(detail, diagnostic)?;
        let text = if job.status == "ng" {
            format!(
                "补试失败，标记 NG · {} · {}",
                job.position.name(),
                job.reason
            )
        } else {
            format!(
                "分页异常，已延后 · {} · {}",
                job.position.name(),
                job.reason
            )
        };
        if self.live {
            let list = self.report["issues"].as_array_mut().unwrap();
            let index = list
                .iter()
                .position(|i| i["type"] == "paging" && i["paging_key"] == key);
            let issue = json!({"stage":self.stage,"target_id":self.target,"time":chrono::Local::now().to_rfc3339(),"type":"paging","reason":job.reason,"retries":job.retries,"resolved":false,"disposition":if job.status=="ng"{"ng"}else{"deferred"},"paging_key":key,"pagination":job.diagnostic});
            if let Some(index) = index {
                list[index] = issue;
            } else {
                list.push(issue);
            }
        }
        // An unresolved risk/network wait must not leak into the next target or become recovered.
        self.waiting = None;
        self.pending_issues.clear();
        if self.live {
            self.report["status"] = json!("running");
        }
        self.sync_paging_report()?;
        (self.emit)(CrawlEvent::Deferred(text.clone()));
        Ok(text)
    }
    pub fn paging_accepted(&mut self) -> Result<()> {
        let recovered = if let Some(p) = &mut self.paging {
            p.accepted()?
        } else {
            None
        };
        if let Some((key, job)) = recovered {
            if self.live {
                let list = self.report["issues"].as_array_mut().unwrap();
                if let Some(issue) = list
                    .iter_mut()
                    .find(|i| i["type"] == "paging" && i["paging_key"] == key)
                {
                    issue["disposition"] = json!("recovered");
                    issue["resolved"] = json!(true);
                    issue["retries"] = json!(1);
                    issue["resolved_at"] = json!(chrono::Local::now().to_rfc3339());
                } else {
                    list.push(json!({"stage":self.stage,"target_id":self.target,"time":chrono::Local::now().to_rfc3339(),"type":"paging","reason":job.reason,"retries":1,"resolved":true,"disposition":"recovered","paging_key":key,"pagination":job.diagnostic}));
                }
            }
            (self.emit)(CrawlEvent::Deferred(format!(
                "补试恢复 · {}",
                job.position.name()
            )));
        }
        self.sync_paging_report()
    }
    fn sync_paging_report(&mut self) -> Result<()> {
        if self.live {
            if let Some(p) = &self.paging {
                let s = p.stats;
                self.report["paging_deferred"] = json!(s.deferred);
                self.report["paging_recovered"] = json!(s.recovered);
                self.report["paging_ng"] = json!(s.ng);
                self.report["paging_skipped_ng"] = json!(s.skipped_ng);
                self.report["paging_unresolved"] = json!(p.unresolved());
            }
            self.save()?;
        }
        Ok(())
    }
    pub fn paging_retry_progress(&mut self, index: usize, job: &paging::Job) -> Result<()> {
        let total = self.paging.as_ref().map(|p| p.remaining()).unwrap_or(0);
        if self.live {
            self.report["status"] = json!("retrying");
            self.sync_paging_report()?;
        }
        (self.emit)(CrawlEvent::Deferred(format!(
            "集中补试 {}/{} · {}",
            index,
            index + total.saturating_sub(1),
            job.position.name()
        )));
        Ok(())
    }
    pub fn finish(&mut self, status: &str, doc: &Doc) -> Result<PathBuf> {
        self.sync_paging_report()?;
        self.report["status"] = json!(status);
        self.report["ended_at"] = json!(chrono::Local::now().to_rfc3339());
        self.report["archived_total"] = json!(doc.items.len());
        let counts: BTreeMap<_, _> = VIEW_TYPES
            .iter()
            .map(|t| (*t, doc.items.iter().filter(|i| item_type(i) == *t).count()))
            .collect();
        self.report["counts"] = json!(counts);
        self.save()?;
        Ok(self.path.clone().unwrap_or_default())
    }
}

pub(super) fn file_summary(store: &Store, task: &Task, group: &str) -> Value {
    let selected: Vec<_> = store
        .doc
        .items
        .iter()
        .filter(|i| {
            if TYPES.contains(&group) {
                i.ty == group
            } else {
                i.ty == "private" && i.session == group
            }
        })
        .collect();
    let done = if group == "likes" {
        task.feeds.get(group).is_some_and(|c| c.done)
            && selected.iter().all(|i| {
                task.likes.get(&i.id).is_some_and(|c| c.is_done())
                    && i.like_detail.status == LikeStatus::Finished
                    && i.like_count == i.like_users.len() as u64
            })
    } else if TYPES.contains(&group) {
        task.feeds.get(group).is_some_and(|c| c.done)
    } else {
        task.sessions.get(group).is_some_and(|s| s.done)
    };
    // A bounded run cannot establish completeness of the entire archive history.
    let complete = done && !task.summary_bounded && task.options.from == 0 && task.options.to == 0;
    json!({"archived_total":selected.len(),"reported_total":null,"status":if complete{"exhausted"}else{"partial"},"exhausted":if complete{json!(true)}else{Value::Null}})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn waiting_uses_fake_clock_and_preserves_semantic_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let root = crate::archive::root(dir.path());
        std::fs::create_dir_all(&root).unwrap();
        let cancel = AtomicBool::new(false);
        let mut events = vec![];
        let mut ticks = 0;
        let mut emit = |e| events.push(e);
        let mut sleep = |_| ticks += 1;
        let mut ctx = CrawlContext::new(&cancel, &mut emit, &mut sleep);
        ctx.start(
            &root,
            12,
            &Options::parse(vec!["likes".into()], "", "").unwrap(),
        )
        .unwrap();
        ctx.progress("点赞第2页".into(), "123");
        ctx.wait(crate::bili::RetryReason::Risk, "风控等待".into())
            .unwrap();
        assert_eq!(ctx.report["issues"][0]["resolved"], false);
        ctx.recovered().unwrap();
        assert_eq!(ctx.report["issues"][0]["resolved"], true);
        let path = ctx.finish("completed", &Doc::new(12)).unwrap();
        drop(ctx);
        assert_eq!(ticks, 240);
        assert!(events
            .iter()
            .any(|e| matches!(e, CrawlEvent::Waiting { .. })));
        assert!(events.iter().any(|e| matches!(e, CrawlEvent::Tick(_))));
        let report: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(report["status"], "completed");
    }
    #[test]
    fn cancellation_during_wait_does_not_mark_recovered() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(crate::archive::root(dir.path())).unwrap();
        let cancel = AtomicBool::new(false);
        let mut emit = |_| {};
        let mut sleep = |_| cancel.store(true, Ordering::SeqCst);
        let mut ctx = CrawlContext::new(&cancel, &mut emit, &mut sleep);
        ctx.start(
            &crate::archive::root(dir.path()),
            12,
            &Options::parse(vec!["likes".into()], "", "").unwrap(),
        )
        .unwrap();
        assert!(ctx
            .wait(crate::bili::RetryReason::Network, "请求超时".into())
            .is_err());
        assert_eq!(ctx.report["issues"][0]["resolved"], false);
    }
}

#[cfg(test)]
mod live_collection_tests {
    use super::*;
    use std::cell::RefCell;
    fn feed() -> Value {
        json!({"code":0,"data":{"latest":{"items":[]},"total":{"items":[{"id":"7","counts":2,"like_time":100,"users":[],"item":{"title":"liked"}}],"cursor":{"is_end":true}}}})
    }
    fn detail(uid: u64, end: bool) -> Value {
        json!({"code":0,"data":{"page":{"is_end":end},"items":[{"user":{"mid":uid,"nickname":"user"},"like_time":90}]}})
    }
    #[test]
    fn live_paging_defers_then_recovers_and_writes_one_resolved_issue() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let store = Arc::new(Mutex::new(Store {
            root: base(dir.path(), 12),
            doc: Doc::new(12),
            ..Default::default()
        }));
        let options = Options::parse(vec!["likes".into()], "", "").unwrap();
        let events = RefCell::new(vec![]);
        let mut emit = |event| events.borrow_mut().push(event);
        let mut clock = 0;
        let mut sleep = |_| clock += 1;
        let mut ctx = CrawlContext::new(&cancel, &mut emit, &mut sleep);
        ctx.start(&store.lock().unwrap().root, 12, &options)
            .unwrap();
        let mut requests = 0;
        let errors = crawl_run(
            &store,
            12,
            options,
            &cancel,
            dir.path().join("task.json"),
            |url| {
                if !url.contains("like_detail") {
                    return Ok(feed());
                }
                requests += 1;
                if requests == 1 {
                    assert!(url.contains("pn=1"));
                    Ok(detail(2, false))
                } else if requests == 2 {
                    assert!(url.contains("pn=2"));
                    Ok(detail(2, false))
                } else {
                    assert!(url.contains("pn=2"));
                    assert!(events.borrow().iter().any(
                        |e| matches!(e, CrawlEvent::Deferred(text) if text.contains("已延后"))
                    ));
                    Ok(detail(3, true))
                }
            },
            |_, _| Ok(()),
            &mut ctx,
        )
        .unwrap();
        assert!(errors.is_empty());
        assert_eq!(store.lock().unwrap().doc.items[0].like_users.len(), 2);
        let report = ctx.finish("completed", &store.lock().unwrap().doc).unwrap();
        drop(ctx);
        assert_eq!(clock, 0);
        let report: Value = serde_json::from_slice(&std::fs::read(report).unwrap()).unwrap();
        assert_eq!(report["issues"].as_array().unwrap().len(), 1);
        assert_eq!(report["issues"][0]["resolved"], true);
        let file = base(dir.path(), 12).join("likes.json");
        let archive: Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
        assert_eq!(archive["collection"]["archived_total"], 1);
        assert!(archive["collection"].get("errors").is_none());
    }
    #[test]
    fn risk_wait_and_login_failure_use_distinct_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = crate::archive::root(dir.path());
        std::fs::create_dir_all(&root).unwrap();
        let cancel = AtomicBool::new(false);
        let mut emit = |_| {};
        let mut clock = 0;
        let mut sleep = |_| clock += 1;
        let mut ctx = CrawlContext::new(&cancel, &mut emit, &mut sleep);
        ctx.start(
            &root,
            12,
            &Options::parse(vec!["likes".into()], "", "").unwrap(),
        )
        .unwrap();
        let mut attempts = 0;
        let value = ctx
            .request(
                &mut |_| {
                    attempts += 1;
                    if attempts < 3 {
                        bail!("HTTP 429")
                    }
                    Ok(feed())
                },
                "fixture",
            )
            .unwrap();
        assert_eq!(value["code"], 0);
        ctx.recovered().unwrap();
        assert!(ctx
            .request(&mut |_| Err(anyhow!("登录失效 -101")), "fixture")
            .is_err());
        drop(ctx);
        assert_eq!(attempts, 3);
        assert_eq!(clock, 480);
    }
    #[test]
    fn a_report_write_failure_stops_before_wait_or_another_request() {
        let dir = tempfile::tempdir().unwrap();
        let root = crate::archive::root(dir.path());
        std::fs::create_dir_all(&root).unwrap();
        let cancel = AtomicBool::new(false);
        let mut events = 0;
        let mut emit = |_| events += 1;
        let mut sleep = |_| panic!("must stop before waiting");
        let mut ctx = CrawlContext::new(&cancel, &mut emit, &mut sleep);
        ctx.start(
            &root,
            12,
            &Options::parse(vec!["likes".into()], "", "").unwrap(),
        )
        .unwrap();
        let reports = root.join("12/reports");
        let original = root.join("12/reports-original");
        std::fs::rename(&reports, &original).unwrap();
        std::fs::write(&reports, b"block directory").unwrap();
        let error = ctx
            .wait(crate::bili::RetryReason::Risk, "风控".into())
            .unwrap_err();
        assert!(error.is::<PersistFailure>());
        assert!(!cancel.load(Ordering::SeqCst));
    }
    #[test]
    fn old_global_diagnostics_are_not_copied_to_each_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut doc = Doc::new(12);
        doc.archive = json!({"collection":{"archived_total":4681,"errors":["unrelated likes error"],"scope":{"types":["likes"]},"status":"exhausted","exhausted":true}});
        for uid in [2, 3] {
            let key = format!("user_{uid}");
            doc.sessions.push(Session {
                key: key.clone(),
                talker_id: uid.to_string(),
                session_type: 1,
                ..Default::default()
            });
            doc.items.push(Item {
                id: uid.to_string(),
                ty: "private".into(),
                session: key,
                sender_uid: uid.to_string(),
                ..Default::default()
            });
        }
        let store = Store {
            root: base(dir.path(), 12),
            doc,
            ..Default::default()
        };
        save(&store).unwrap();
        for uid in [2, 3] {
            let path = store.root.join(format!("private/user_{uid}.json"));
            let value: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            assert_eq!(value["collection"]["archived_total"], 1);
            assert_eq!(value["collection"]["exhausted"], Value::Null);
            assert!(value["collection"].get("errors").is_none());
            assert!(value["collection"].get("scope").is_none());
        }
        assert!(
            store.doc.archive["collection"]["errors"].is_array(),
            "saving does not rewrite the source in memory"
        );
    }
}

#[cfg(test)]
mod paging_queue_tests {
    use super::*;
    fn fixture(types: &[&str]) -> (tempfile::TempDir, Arc<Mutex<Store>>, Options, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("12.json");
        let store = Arc::new(Mutex::new(Store {
            root: base(dir.path(), 12),
            doc: Doc::new(12),
            ..Default::default()
        }));
        let options =
            Options::parse(types.iter().map(|t| t.to_string()).collect(), "", "").unwrap();
        (dir, store, options, path)
    }
    fn feed() -> Value {
        json!({"code":0,"data":{"latest":{"items":[]},"total":{"items":[{"id":"7","counts":3,"like_time":100,"users":[],"item":{"title":"first"}},{"id":"8","counts":1,"like_time":99,"users":[],"item":{"title":"second"}}],"cursor":{"is_end":true}}}})
    }
    fn empty(end: bool) -> Value {
        json!({"code":0,"data":{"page":{"is_end":end},"items":[]}})
    }
    fn detail() -> Value {
        json!({"code":0,"data":{"page":{"is_end":true},"items":[{"user":{"mid":2,"nickname":"user"},"like_time":90}]}})
    }
    #[test]
    fn failed_notice_waits_until_other_notices_finish_then_becomes_durable_ng() {
        let (_dir, store, options, path) = fixture(&["likes"]);
        let cancel = AtomicBool::new(false);
        let mut events = vec![];
        let mut emit = |e| events.push(e);
        let mut sleep = |_| panic!("paging must not add a wait");
        let mut ctx = CrawlContext::new(&cancel, &mut emit, &mut sleep);
        ctx.start(&store.lock().unwrap().root, 12, &options)
            .unwrap();
        let mut calls = vec![];
        let errors = crawl_run(
            &store,
            12,
            options.clone(),
            &cancel,
            path.clone(),
            |url| {
                if !url.contains("like_detail") {
                    return Ok(feed());
                }
                if url.contains("card_id=7&") {
                    calls.push(7);
                    Ok(empty(false))
                } else {
                    calls.push(8);
                    Ok(detail())
                }
            },
            |_, _| Ok(()),
            &mut ctx,
        )
        .unwrap();
        assert!(!errors.is_empty());
        assert_eq!(calls, [7, 8, 7]);
        assert_eq!(
            (
                ctx.paging.as_ref().unwrap().stats.deferred,
                ctx.paging.as_ref().unwrap().stats.ng
            ),
            (1, 1)
        );
        let report = ctx.finish("partial", &store.lock().unwrap().doc).unwrap();
        drop(ctx);
        let report: Value = serde_json::from_slice(&std::fs::read(report).unwrap()).unwrap();
        assert_eq!(report["issues"][0]["retries"], 1);
        assert_eq!(report["issues"][0]["disposition"], "ng");
        assert_eq!(report["paging_ng"], 1);
        assert!(events
            .iter()
            .any(|e| matches!(e,CrawlEvent::Deferred(t) if t.contains("集中补试"))));
        let mut emit = |_| {};
        let mut sleep = |_| {};
        let mut ctx = CrawlContext::new(&cancel, &mut emit, &mut sleep);
        let errors = crawl_run(
            &store,
            12,
            options.clone(),
            &cancel,
            path.clone(),
            |url| {
                if !url.contains("like_detail") {
                    return Ok(feed());
                }
                assert!(url.contains("card_id=8&"));
                Ok(detail())
            },
            |_, _| Ok(()),
            &mut ctx,
        )
        .unwrap();
        assert!(!errors.is_empty());
        assert_eq!(ctx.paging.as_ref().unwrap().stats.skipped_ng, 1);
        let original = std::fs::read(&path).unwrap();
        let mut emit = |_| {};
        let mut sleep = |_| {};
        let mut ctx = CrawlContext::new(&cancel, &mut emit, &mut sleep);
        ctx.manual_paging = true;
        let mut retried = 0;
        let errors = crawl_run(
            &store,
            12,
            Options::parse(VIEW_TYPES.iter().map(|t| t.to_string()).collect(), "", "").unwrap(),
            &cancel,
            path.with_extension("ng.json"),
            |url| {
                assert!(url.contains("card_id=7&"));
                retried += 1;
                Ok(detail())
            },
            |_, _| panic!("manual NG must not process unrelated attachments"),
            &mut ctx,
        )
        .unwrap();
        assert!(errors.is_empty());
        assert_eq!(retried, 1);
        assert_eq!(ctx.paging.as_ref().unwrap().stats.recovered, 1);
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
    #[test]
    fn feed_failures_are_deferred_across_all_selected_sources() {
        for source in ["replies", "mentions", "likes"] {
            let (_dir, store, options, path) = fixture(&[source]);
            let cancel = AtomicBool::new(false);
            let mut emit = |_| {};
            let mut sleep = |_| panic!("no paging wait");
            let mut ctx = CrawlContext::new(&cancel, &mut emit, &mut sleep);
            let mut count = 0;
            let errors = crawl_run(
                &store,
                12,
                options,
                &cancel,
                path,
                |_| {
                    count += 1;
                    let body = json!({"items":[],"cursor":{"is_end":false}});
                    Ok(if source == "likes" {
                        json!({"code":0,"data":{"latest":{"items":[]},"total":body}})
                    } else {
                        json!({"code":0,"data":body})
                    })
                },
                |_, _| Ok(()),
                &mut ctx,
            )
            .unwrap();
            assert_eq!(count, 2);
            assert!(!errors.is_empty());
            assert_eq!(ctx.paging.as_ref().unwrap().stats.ng, 1);
        }
    }
    #[test]
    fn session_lists_and_user_and_group_histories_share_the_budget() {
        for source in ["private", "groups"] {
            let (_dir, store, options, path) = fixture(&[source]);
            let cancel = AtomicBool::new(false);
            let mut emit = |_| {};
            let mut sleep = |_| panic!("no paging wait");
            let mut ctx = CrawlContext::new(&cancel, &mut emit, &mut sleep);
            let mut list_calls = 0;
            let mut history_calls = 0;
            let session_type = if source == "groups" { 2 } else { 1 };
            let errors=crawl_run(&store,12,options,&cancel,path,|url|{
                if url.contains("get_sessions") {list_calls+=1;return Ok(json!({"code":0,"data":{"has_more":false,"session_list":[{"talker_id":"2","session_type":session_type,"session_ts":100,"account_info":{"name":"person"}}]}}));}
                assert!(url.contains("fetch_session_msgs"));history_calls+=1;Ok(json!({"code":0,"data":{"has_more":true,"messages":[]}}))
            },|_,_|Ok(()),&mut ctx).unwrap();
            assert!(list_calls > 0);
            assert_eq!(history_calls, 2);
            assert!(!errors.is_empty());
            assert_eq!(ctx.paging.as_ref().unwrap().stats.ng, 1);
            let (_dir, store, options, path) = fixture(&[source]);
            let mut ctx = CrawlContext::new(&cancel, &mut emit, &mut sleep);
            let mut calls = 0;
            let categories = options.session_categories().len();
            crawl_run(
                &store,
                12,
                options,
                &cancel,
                path,
                |url| {
                    assert!(url.contains("get_sessions"));
                    calls += 1;
                    Ok(json!({"code":0,"data":{"has_more":true,"session_list":[]}}))
                },
                |_, _| Ok(()),
                &mut ctx,
            )
            .unwrap();
            assert_eq!(calls, 2 * categories);
            assert_eq!(ctx.paging.as_ref().unwrap().stats.ng, categories);
        }
    }
    #[test]
    fn risk_waits_during_supplement_do_not_consume_the_paging_retry() {
        let (_dir, store, options, path) = fixture(&["likes"]);
        let cancel = AtomicBool::new(false);
        let mut emit = |_| {};
        let mut ticks = 0;
        let mut sleep = |_| ticks += 1;
        let mut ctx = CrawlContext::new(&cancel, &mut emit, &mut sleep);
        ctx.start(&store.lock().unwrap().root, 12, &options)
            .unwrap();
        let mut stalled_calls = 0;
        let errors = crawl_run(
            &store,
            12,
            options,
            &cancel,
            path,
            |url| {
                if !url.contains("like_detail") {
                    return Ok(feed());
                }
                if url.contains("card_id=8&") {
                    return Ok(detail());
                }
                stalled_calls += 1;
                match stalled_calls {
                    1 => Ok(empty(false)),
                    2 => bail!("HTTP 429"),
                    _ => Ok(detail()),
                }
            },
            |_, _| Ok(()),
            &mut ctx,
        )
        .unwrap();
        assert!(errors.is_empty());
        assert_eq!(stalled_calls, 3);
        assert_eq!(ctx.paging.as_ref().unwrap().stats.recovered, 1);
        assert_eq!(ctx.paging.as_ref().unwrap().stats.ng, 0);
        drop(ctx);
        assert_eq!(ticks, 240);
    }
    #[test]
    fn cancellation_after_deferral_keeps_pending_without_starting_supplement() {
        let (_dir, store, options, path) = fixture(&["likes"]);
        let cancel = AtomicBool::new(false);
        let mut emit = |e| {
            if matches!(e,CrawlEvent::Deferred(t) if t.contains("已延后")) {
                cancel.store(true, Ordering::SeqCst);
            }
        };
        let mut sleep = |_| {};
        let mut ctx = CrawlContext::new(&cancel, &mut emit, &mut sleep);
        let mut calls = 0;
        crawl_run(
            &store,
            12,
            options.clone(),
            &cancel,
            path.clone(),
            |url| {
                if !url.contains("like_detail") {
                    return Ok(feed());
                }
                calls += 1;
                Ok(empty(false))
            },
            |_, _| Ok(()),
            &mut ctx,
        )
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(ctx.paging.as_ref().unwrap().stats.ng, 0);
        drop(ctx);
        cancel.store(false, Ordering::SeqCst);
        let mut emit = |_| {};
        let mut sleep = |_| {};
        let mut ctx = CrawlContext::new(&cancel, &mut emit, &mut sleep);
        let mut calls = vec![];
        let errors = crawl_run(
            &store,
            12,
            options,
            &cancel,
            path,
            |url| {
                assert!(url.contains("like_detail"));
                calls.push(if url.contains("card_id=7&") { 7 } else { 8 });
                Ok(detail())
            },
            |_, _| Ok(()),
            &mut ctx,
        )
        .unwrap();
        assert!(errors.is_empty());
        assert_eq!(calls, [8, 7]);
        assert_eq!(ctx.paging.as_ref().unwrap().stats.recovered, 1);
    }
    #[test]
    fn explicit_end_and_count_discrepancies_are_not_paging_errors() {
        let (_dir, store, options, path) = fixture(&["likes"]);
        let cancel = AtomicBool::new(false);
        let mut emit = |_| {};
        let mut sleep = |_| {};
        let mut ctx = CrawlContext::new(&cancel, &mut emit, &mut sleep);
        let errors = crawl_run(
            &store,
            12,
            options,
            &cancel,
            path,
            |url| {
                Ok(if url.contains("like_detail") {
                    empty(true)
                } else {
                    feed()
                })
            },
            |_, _| Ok(()),
            &mut ctx,
        )
        .unwrap();
        assert!(errors.is_empty());
        assert_eq!(ctx.paging.as_ref().unwrap().stats.ng, 0);
        assert!(store
            .lock()
            .unwrap()
            .doc
            .items
            .iter()
            .all(|i| i.like_detail.status == LikeStatus::Finished));
    }
}
