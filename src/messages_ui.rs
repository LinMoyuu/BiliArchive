//! Slint integration for the independent account archive workspace.
use crate::*;
use std::sync::mpsc::Sender;

pub enum Action {
    Load(u64),
    Attachments(u64),
    Collect(u64, messages::Options),
    RetryNg(u64),
    Export(u64, PathBuf),
}
// Login identity is authoritative before the asynchronous header profile is published.
// Checking AppWindow.account-uid here would discard startup state while that label is empty.
fn snapshot_is_current(shared: &Shared, uid: u64, root: &std::path::Path) -> bool {
    if shared.account_uid.load(Ordering::SeqCst) != uid {
        return false;
    }
    uid == 0
        || messages::base(
            &PathBuf::from(export_dir_of(&shared.cfg.lock().unwrap())),
            uid,
        ) == root
}
// Workspace switching only queues this bounded request; all store/file access is worker-side.
pub fn request_view(shared: &Shared, tx: &Sender<Cmd>) -> bool {
    let uid = shared.account_uid.load(Ordering::SeqCst);
    if uid == 0 {
        return false;
    }
    loop {
        let queued = shared.message_view_pending.load(Ordering::SeqCst);
        if queued == uid {
            return false;
        }
        if shared
            .message_view_pending
            .compare_exchange(queued, uid, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            break;
        }
    }
    if tx.send(Cmd::EnsureMessageView { uid }).is_err() {
        let _ = shared.message_view_pending.compare_exchange(
            uid,
            0,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        return false;
    }
    true
}
pub fn ensure_view(uid: u64, shared: &Arc<Shared>, ui: &Weak<AppWindow>) {
    struct Finish<'a>(&'a Shared, u64);
    impl Drop for Finish<'_> {
        fn drop(&mut self) {
            let _ = self.0.message_view_pending.compare_exchange(
                self.1,
                0,
                Ordering::SeqCst,
                Ordering::SeqCst,
            );
        }
    }
    let _finish = Finish(shared, uid);
    if uid == 0
        || shared.account_uid.load(Ordering::SeqCst) != uid
        || shared.message_view_pending.load(Ordering::SeqCst) != uid
    {
        return;
    }
    let root = messages::base(
        &PathBuf::from(export_dir_of(&shared.cfg.lock().unwrap())),
        uid,
    );
    let loaded = {
        let state = shared.messages.lock().unwrap();
        state.doc.account_uid == uid.to_string() && state.root == root
    };
    // The mounted view already holds its last published statistics. Do not parse
    // archives, checkpoints or NG again merely because the user switches tabs.
    if !loaded {
        run(Action::Load(uid), shared, ui);
    }
}

pub fn refresh(shared: &Arc<Shared>, ui: &Weak<AppWindow>) {
    let uid = shared.account_uid.load(Ordering::SeqCst);
    let state = shared.messages.lock().unwrap();
    let current = uid != 0 && state.doc.account_uid == uid.to_string();
    let counts = if current {
        state.doc.counts()
    } else {
        Default::default()
    };
    let values = ["replies", "mentions", "likes", "private", "groups"]
        .map(|ty| counts.get(ty).copied().unwrap_or(0).to_string());
    let (saved, pending) = if current {
        archive::message_resource_counts(&state.doc)
    } else {
        (0, 0)
    };
    let ng = if current {
        messages::ng_count(uid, &state.root.to_string_lossy()).unwrap_or(0)
    } else {
        0
    };
    let has_resume = current && messages::has_resumable_task(uid, &state).unwrap_or(false);
    let snapshot_root = state.root.clone();
    drop(state);
    publish_stats(shared, ui, uid, snapshot_root, values, saved, pending, ng, has_resume);
}

pub fn load_overview(shared: &Arc<Shared>, ui: &Weak<AppWindow>, uid: u64) {
    let root = PathBuf::from(export_dir_of(&shared.cfg.lock().unwrap()));
    let (summary, has_resume) = crate::preview_disk::desktop_message_view(&root, uid);
    let root = messages::base(&root, uid);
    if !snapshot_is_current(shared, uid, &root) { return; }
    let values = ["replies", "mentions", "likes", "private", "groups"]
        .map(|ty| summary["counts"][ty].as_u64().unwrap_or(0).to_string());
    let saved = summary["attachments_saved"].as_u64().unwrap_or(0) as usize;
    let pending = summary["attachments_pending"].as_u64().unwrap_or(0) as usize;
    let ng = messages::ng_count(uid, &root.to_string_lossy()).unwrap_or(0);
    *shared.messages.lock().unwrap() = messages::Store {
        doc: messages::Doc::new(uid),
        root: root.clone(),
        notice: String::new(),
    };
    publish_stats(shared, ui, uid, root, values, saved, pending, ng, has_resume);
}

fn publish_stats(shared: &Arc<Shared>, ui: &Weak<AppWindow>, uid: u64, snapshot_root: PathBuf,
    values: [String; 5], saved: usize, pending: usize, ng: usize, has_resume: bool,
) {
    let account = shared.clone();
    emit(ui, move |w| {
        if !snapshot_is_current(&account, uid, &snapshot_root) {
            return;
        }
        let [replies, mentions, likes, private, groups] = values;
        w.set_message_count_replies(replies.into());
        w.set_message_count_mentions(mentions.into());
        w.set_message_count_likes(likes.into());
        w.set_message_count_private(private.into());
        w.set_message_count_groups(groups.into());
        w.set_message_has_resume(has_resume);
        w.set_message_ng_count(ng.min(i32::MAX as usize) as i32);
        w.set_message_attachments_pending(pending.min(i32::MAX as usize) as i32);
        w.set_message_attachments_status(
            format!("附件 {} 已保存 / {} 待处理", saved, pending).into(),
        );
    });
}
fn task_state(shared: &Arc<Shared>, ui: &Weak<AppWindow>, text: String, ratio: f32) {
    push_log(shared, ui, &text);
    set_progress(ui, ratio, text.clone());
    set_foot(ui, text.clone());
}
pub fn run(action: Action, shared: &Arc<Shared>, ui: &Weak<AppWindow>) {
    let uid = match &action {
        Action::Load(u)
        | Action::Attachments(u)
        | Action::Collect(u, _)
        | Action::RetryNg(u)
        | Action::Export(u, _) => *u,
    };
    if uid == 0 || uid != shared.account_uid.load(Ordering::SeqCst) {
        set_foot(ui, "请先登录当前账号".into());
        return;
    }
    if shared.offline.load(Ordering::SeqCst)
        && matches!(&action, Action::Collect(..) | Action::RetryNg(_))
    {
        set_foot(
            ui,
            "离线账号仅可读取本地内容和处理公开附件，分页 NG 需在线验证原账号".into(),
        );
        return;
    }
    if matches!(&action, Action::Load(_)) {
        let _reading = LocalReadGuard::messages(shared, ui);
        load_overview(shared, ui, uid);
        return;
    }
    let retry_ng = matches!(&action, Action::RetryNg(_));
    let collecting = matches!(&action, Action::Collect(..) | Action::RetryNg(_));
    set_busy(shared, ui, true);
    let cancellable = collecting || matches!(&action, Action::Attachments(_));
    let _task = TaskGuard::begin(
        shared,
        ui,
        if collecting {
            2
        } else if cancellable {
            3
        } else {
            0
        },
    );
    shared.message_active.store(cancellable, Ordering::SeqCst);
    emit(ui, move |w| w.set_message_active(cancellable));
    if !cancellable {
        shared.cancel.store(false, Ordering::SeqCst);
        shared.pause_requested.store(false, Ordering::SeqCst);
    }
    let mut issue_logged = false;
    let mut report_path: Option<PathBuf> = None;
    let result = (|| -> Result<()> {
        let root = PathBuf::from(export_dir_of(&shared.cfg.lock().unwrap()));
        // Always merge disk before doing work so repeated sessions preserve archives.
        {
            let _guard = LocalReadGuard::messages(shared, ui);
            let loaded = messages::load(&root, uid)?;
            if !loaded.notice.is_empty() {
                push_log(shared, ui, &loaded.notice);
            }
            *shared.messages.lock().unwrap() = loaded;
        }
        refresh(shared, ui);
        let action = if retry_ng {
            Action::Collect(
                uid,
                messages::Options::parse(
                    messages::VIEW_TYPES.iter().map(|t| t.to_string()).collect(),
                    "",
                    "",
                )?,
            )
        } else {
            action
        };
        match action {
            Action::Load(_) => {}
            Action::RetryNg(_) => unreachable!("NG action is normalized before dispatch"),
            Action::Collect(_, options) => {
                let client = make_client(shared)?;
                let cookie = shared.cfg.lock().unwrap().sessdata.clone();
                task_state(
                    shared,
                    ui,
                    "消息采集开始／继续，正在读取历史记录".into(),
                    0.0,
                );
                let mut progress_at = std::time::Instant::now() - Duration::from_secs(1);
                let mut preview_at = std::time::Instant::now();
                let mut previous = String::new();
                let mut waiting_reason = String::new();
                let outcome = messages::crawl_events_mode(
                    &shared.messages,
                    uid,
                    options,
                    &shared.cancel,
                    &shared.pause_requested,
                    |url| {
                        if shared.cfg.lock().unwrap().sessdata != cookie
                            || shared.account_uid.load(Ordering::SeqCst) != uid
                        {
                            anyhow::bail!("账号已变更，原进度已保留");
                        }
                        messages::pause(client.next_pause(), &shared.cancel)?;
                        client.message_request_once(url)
                    },
                    |url, path| {
                        messages::pause(client.next_pause(), &shared.cancel)?;
                        messages::download(url, path, &shared.cancel)
                    },
                    |event| match event {
                        messages::CrawlEvent::Stage(text) => {
                            previous = text.clone();
                            progress_at = std::time::Instant::now();
                            task_state(shared, ui, text, 0.0);
                        }
                        messages::CrawlEvent::Progress(text) => {
                            if progress_at.elapsed() >= Duration::from_millis(250)
                                && previous != text
                            {
                                previous = text.clone();
                                push_log(shared, ui, &text);
                                let display = if waiting_reason.is_empty() {
                                    text.clone()
                                } else {
                                    format!("正在重试：{}", text)
                                };
                                set_progress(ui, 0.0, display.clone());
                                progress_at = std::time::Instant::now();
                            }
                            // Keep duplicate progress quiet without suppressing state transitions.
                            if preview_at.elapsed() >= Duration::from_secs(1) {
                                refresh(shared, ui);
                                preview_at = std::time::Instant::now();
                            }
                        }
                        messages::CrawlEvent::Waiting {
                            reason,
                            detail,
                            seconds,
                        } => {
                            waiting_reason = reason.clone();
                            let state = format!("{} · {} · 等待 {} 秒", reason, detail, seconds);
                            task_state(shared, ui, state, 0.0);
                            emit(ui, move |w| {
                                w.set_verify_title(reason.clone().into());
                                w.set_verify_wait_label(reason.into());
                            });
                            verify_show(ui, seconds, detail);
                        }
                        messages::CrawlEvent::Tick(left) => {
                            verify_tick(ui, left);
                        }
                        messages::CrawlEvent::Retrying(text) => {
                            task_state(shared, ui, format!("正在重试：{}", text), 0.0)
                        }

                        messages::CrawlEvent::Recovered(text) => {
                            waiting_reason.clear();
                            verify_hide(ui);
                            task_state(shared, ui, format!("请求与分页已恢复：{}", text), 0.0);
                        }
                        messages::CrawlEvent::Deferred(text) => {
                            waiting_reason.clear();
                            verify_hide(ui);
                            task_state(shared, ui, text, 0.0);
                        }
                        messages::CrawlEvent::Issue(text) => {
                            issue_logged = true;
                            push_log(shared, ui, text);
                        }
                        messages::CrawlEvent::Report(path) => {
                            push_log(shared, ui, format!("采集报告：{}", path.display()));
                            report_path = Some(path);
                        }
                    },
                    retry_ng,
                )?;
                let final_text = if shared.cancel.load(Ordering::SeqCst) {
                    if shared.pause_requested.load(Ordering::SeqCst) {
                        "消息采集已暂停，进度与归档已保留".to_owned()
                    } else {
                        if !retry_ng {
                            messages::clear_task(uid)?;
                        }
                        "消息采集已结束，已保存的消息与附件保留".to_owned()
                    }
                } else if outcome.errors.is_empty() {
                    format!(
                        "消息采集完成 · 分页补试恢复 {} 项",
                        outcome.paging.recovered
                    )
                } else {
                    let other = outcome
                        .errors
                        .len()
                        .saturating_sub(usize::from(outcome.paging_unresolved > 0));
                    format!("消息采集部分完成 · 补试恢复 {} 项 · 新增 NG {} 项 · 跳过已有 NG {} 项 · 未解决分页 {} 项 · 其他异常 {} 项；NG 可使用「重试NG项」补采",outcome.paging.recovered,outcome.paging.ng,outcome.paging.skipped_ng,outcome.paging_unresolved,other)
                };
                let complete = !shared.cancel.load(Ordering::SeqCst) && outcome.errors.is_empty();
                task_state(shared, ui, final_text, if complete { 1.0 } else { 0.0 });
                push_log(
                    shared,
                    ui,
                    format!("本次报告：{}", outcome.report.display()),
                );
            }

            Action::Attachments(_) => {
                task_state(shared, ui, "开始处理账号消息附件".into(), 0.0);
                let mut logged_errors = 0;
                let (min, max) = {
                    let cfg = shared.cfg.lock().unwrap();
                    (
                        cfg.interval_ms.max(0) as u64,
                        cfg.interval_max_ms.max(0) as u64,
                    )
                };
                let report = messages::process_configured_attachments(
                    &shared.messages,
                    uid,
                    &shared.cancel,
                    |_| true,
                    |url, path| {
                        if shared.account_uid.load(Ordering::SeqCst) != uid {
                            shared.cancel.store(true, Ordering::SeqCst);
                            anyhow::bail!("账号已变更，原附件进度已保留");
                        }
                        messages::pause(
                            std::time::Duration::from_millis(bili::random_delay_ms(min, max)),
                            &shared.cancel,
                        )?;
                        messages::download(url, path, &shared.cancel)
                    },
                    |report, key| {
                        for error in report.errors.iter().skip(logged_errors) {
                            push_log(shared, ui, error);
                        }
                        logged_errors = report.errors.len();
                        set_progress(
                            ui,
                            if report.total == 0 {
                                1.0
                            } else {
                                report.processed as f32 / report.total as f32
                            },
                            format!(
                                "附件 {}/{} · 成功 {} / 失败 {} · {}",
                                report.processed, report.total, report.saved, report.failed, key
                            ),
                        );
                        refresh(shared, ui);
                        Ok(())
                    },
                )?;
                let label = if shared.cancel.load(Ordering::SeqCst) {
                    if shared.pause_requested.load(Ordering::SeqCst) {
                        "已暂停"
                    } else {
                        "已结束"
                    }
                } else {
                    "已完成"
                };
                set_progress(
                    ui,
                    if report.total == 0 {
                        1.0
                    } else {
                        report.processed as f32 / report.total as f32
                    },
                    format!(
                        "附件处理{} · {}/{} · 成功 {} / 失败 {}",
                        label, report.processed, report.total, report.saved, report.failed
                    ),
                );
                task_state(
                    shared,
                    ui,
                    format!(
                        "附件处理{} · 成功 {} / 失败 {}；剩余附件可再次处理",
                        label, report.saved, report.failed
                    ),
                    if report.total == 0 {
                        1.0
                    } else {
                        report.processed as f32 / report.total as f32
                    },
                );
            }
            Action::Export(_, path) => {
                let _guard = LocalReadGuard::messages(shared, ui);
                messages::export_bundle(&shared.messages.lock().unwrap(), &path)?;
                set_foot(ui, format!("消息与附件已导出：{}", path.display()));
            }
        }
        Ok(())
    })();
    let preserve_task = result
        .as_ref()
        .err()
        .is_some_and(messages::is_persist_failure);
    if let Err(e) = result {
        let cancelled = shared.cancel.load(Ordering::SeqCst) && !messages::is_persist_failure(&e);
        if cancellable {
            let label = if collecting {
                "消息采集"
            } else {
                "附件处理"
            };
            let text = if cancelled {
                if shared.pause_requested.load(Ordering::SeqCst) {
                    format!("{}已暂停，进度与归档已保留", label)
                } else {
                    format!("{}已结束，已保存的数据保留", label)
                }
            } else if issue_logged {
                format!("{}失败，进度已保留", label)
            } else {
                format!("{}失败，进度已保留：{e:#}", label)
            };
            task_state(shared, ui, text, 0.0);
        } else {
            push_log(shared, ui, format!("账号消息：{e:#}"));
            set_foot(ui, format!("账号消息：{e:#}"));
        }
        if let Some(path) = report_path {
            push_log(shared, ui, format!("本次报告：{}", path.display()));
        }
    }
    // Ending must clear the message task even if cancellation surfaced as an error.
    if collecting
        && !retry_ng
        && !preserve_task
        && shared.cancel.load(Ordering::SeqCst)
        && !shared.pause_requested.load(Ordering::SeqCst)
    {
        if let Err(e) = messages::clear_task(uid) {
            push_log(shared, ui, e.to_string());
        }
    }
    verify_hide(ui);
    shared.message_active.store(false, Ordering::SeqCst);
    emit(ui, |w| w.set_message_active(false));
    set_busy(shared, ui, false);
    refresh(shared, ui);
}
pub fn install(
    ui: &AppWindow,
    shared: &Arc<Shared>,
    tx: &Sender<Cmd>,
    weak: &Weak<AppWindow>,
    web: &Arc<Mutex<Option<WebHandle>>>,
) {
    crate::message_dates::install(ui, weak);
    {
        let shared = shared.clone();
        let tx = tx.clone();
        ui.on_message_retry_ng(move || {
            let uid = shared.account_uid.load(Ordering::SeqCst);
            if uid == 0 || shared.busy.load(Ordering::SeqCst) {
                return;
            }
            let _ = tx.send(Cmd::Messages(Action::RetryNg(uid)));
        });
    }
    {
        let shared = shared.clone();
        let tx = tx.clone();
        ui.on_message_attachments(move || {
            if !shared.busy.load(Ordering::SeqCst) {
                let uid = shared.account_uid.load(Ordering::SeqCst);
                if uid > 0 {
                    let _ = tx.send(Cmd::Messages(Action::Attachments(uid)));
                }
            }
        });
    }

    {
        let shared = shared.clone();
        let tx = tx.clone();
        let weak = weak.clone();
        ui.on_workspace_chosen(move |workspace| {
            let Some(ui) = weak.upgrade() else { return };
            if !(0..=1).contains(&workspace) || ui.get_workspace() == workspace {
                return;
            }
            ui.set_workspace(workspace);
            if workspace == 1 {
                if !shared.busy.load(Ordering::SeqCst) {
                    crate::message_dates::refresh_automatic_end(&ui);
                }
                request_view(&shared, &tx);
            }
        });
    }

    {
        let shared = shared.clone();
        let tx = tx.clone();
        let weak = weak.clone();
        ui.on_message_load(move || {
            if let Some(ui) = weak.upgrade() {
                crate::message_dates::refresh_automatic_end(&ui);
            }
            let uid = shared.account_uid.load(Ordering::SeqCst);
            let _ = tx.send(Cmd::Messages(Action::Load(uid)));
        });
    }
    {
        let shared = shared.clone();
        let tx = tx.clone();
        let weak = weak.clone();
        ui.on_message_start(move || {
            if shared.busy.load(Ordering::SeqCst) {
                return;
            }
            let Some(w) = weak.upgrade() else { return };
            crate::message_dates::refresh_automatic_end(&w);
            if !crate::message_dates::validate(&w) {
                return;
            }
            let types = [
                ("private", w.get_message_private()),
                ("groups", w.get_message_groups()),
                ("likes", w.get_message_likes()),
                ("replies", w.get_message_replies()),
                ("mentions", w.get_message_mentions()),
            ]
            .into_iter()
            .filter(|(_, enabled)| *enabled)
            .map(|(t, _)| t.to_string())
            .collect();
            match messages::Options::parse(
                types,
                w.get_message_from().as_str(),
                w.get_message_to().as_str(),
            ) {
                Ok(o) => {
                    let _ = tx.send(Cmd::Messages(Action::Collect(
                        shared.account_uid.load(Ordering::SeqCst),
                        o,
                    )));
                }
                Err(e) => set_foot(&weak, e.to_string()),
            }
        });
    }

    {
        let shared = shared.clone();
        let tx = tx.clone();
        ui.on_message_export(move || {
            if shared.busy.load(Ordering::SeqCst) {
                return;
            }
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("消息 JSON", &["json"])
                .set_file_name("messages.json")
                .save_file()
            {
                let _ = tx.send(Cmd::Messages(Action::Export(
                    shared.account_uid.load(Ordering::SeqCst),
                    path,
                )));
            }
        });
    }
    {
        let shared = shared.clone();
        let weak = weak.clone();
        let web = web.clone();
        ui.on_message_web(move || {
            let port = shared.cfg.lock().unwrap().web_port;
            ensure_web(&shared, &weak, &web, port, true);
        });
    }
}

#[cfg(test)]
mod startup_state_tests {
    use super::*;
    #[test]
    fn startup_snapshot_does_not_depend_on_header_or_avatar_readiness() {
        let temp = tempfile::tempdir().unwrap();
        let mut cfg = Cfg::default();
        cfg.export_dir = temp.path().to_string_lossy().into_owned();
        let shared = Shared::new(cfg);
        shared.account_uid.store(12, Ordering::SeqCst);
        let root = messages::base(temp.path(), 12);
        assert!(!shared.public_account.lock().unwrap().logged_in);
        assert!(snapshot_is_current(&shared, 12, &root));
        shared.public_account.lock().unwrap().logged_in = true;
        assert!(snapshot_is_current(&shared, 12, &root));
    }
    #[test]
    fn old_account_and_directory_snapshots_still_cannot_overwrite_current_state() {
        let temp = tempfile::tempdir().unwrap();
        let mut cfg = Cfg::default();
        cfg.export_dir = temp.path().to_string_lossy().into_owned();
        let shared = Shared::new(cfg);
        shared.account_uid.store(12, Ordering::SeqCst);
        let root = messages::base(temp.path(), 12);
        shared.account_uid.store(13, Ordering::SeqCst);
        assert!(!snapshot_is_current(&shared, 12, &root));
        shared.account_uid.store(12, Ordering::SeqCst);
        shared.cfg.lock().unwrap().export_dir =
            temp.path().join("other").to_string_lossy().into_owned();
        assert!(!snapshot_is_current(&shared, 12, &root));
        shared.account_uid.store(0, Ordering::SeqCst);
        assert!(snapshot_is_current(&shared, 0, std::path::Path::new("")));
        assert!(!snapshot_is_current(&shared, 12, &root));
    }

    #[test]
    fn view_requests_are_nonblocking_and_coalesce_while_source_locks_are_held() {
        let shared = Shared::new(Cfg::default());
        shared.account_uid.store(12, Ordering::SeqCst);
        let (tx, rx) = std::sync::mpsc::channel();
        let _source = shared.messages.lock().unwrap();
        let _cfg = shared.cfg.lock().unwrap();
        assert!(request_view(&shared, &tx));
        for _ in 0..100 {
            assert!(!request_view(&shared, &tx));
        }
        assert!(matches!(
            rx.try_recv().unwrap(),
            Cmd::EnsureMessageView { uid: 12 }
        ));
        assert!(rx.try_recv().is_err());
        shared.account_uid.store(13, Ordering::SeqCst);
        assert!(request_view(&shared, &tx));
        assert!(matches!(
            rx.try_recv().unwrap(),
            Cmd::EnsureMessageView { uid: 13 }
        ));
    }
    #[test]
    fn unavailable_worker_and_logged_out_requests_do_not_leave_pending_flags() {
        let shared = Shared::new(Cfg::default());
        let (tx, rx) = std::sync::mpsc::channel();
        assert!(!request_view(&shared, &tx));
        assert_eq!(shared.message_view_pending.load(Ordering::SeqCst), 0);
        shared.account_uid.store(12, Ordering::SeqCst);
        drop(rx);
        assert!(!request_view(&shared, &tx));
        assert_eq!(shared.message_view_pending.load(Ordering::SeqCst), 0);
    }
}
