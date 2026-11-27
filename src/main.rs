#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod account_ui;
mod library;
mod local_read;
mod offline;
mod my_comments;
mod portable_zip;
mod archive;
mod bili;
mod checkpoint;
mod collection;
mod range_ui;
mod content_rows;
mod export;
mod model;
mod messages;
mod message_groups;
mod message_subjects;
mod message_profiles;
mod messages_ui;
mod message_dates;
mod qrlogin;
mod secure;
mod single_instance;
mod web;
mod web_images;
mod preview_disk;
mod preview_process;

use anyhow::{Context, Result};
use model::{human_count, ts_text, ArchiveDoc, Video};
use slint::{ModelRc, SharedString, VecModel, Weak};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

slint::include_modules!();

const MAX_LOG_LINES: usize = 240;

// ---------------- 配置 ----------------
#[derive(Clone)]
struct Cfg {
    secret_snapshot: secure::Secrets,
    secrets_blocked: bool,
    clear_login: bool,
    /// 整套 Cookie（扫码登录后是 SESSDATA + bili_jct + DedeUserID… 全套）
    sessdata: String,
    /// 设备指纹，扫码或首次请求时拿到，之后一直复用
    buvid3: String,
    buvid4: String,
    include_replies: bool,
    /// 是否一并采集动态 / 转发动态 / 专栏 / 音频的评论与原文
    include_dynamics: bool,
    ranges: collection::Ranges,
    /// 翻页间隔下限（毫秒）。旧配置只有这一项，作为单点值兼容
    interval_ms: i64,
    /// 翻页间隔上限（毫秒）。每次翻页在 [interval_ms, interval_max_ms] 内随机；
    /// 小于等于下限则退化为固定值
    interval_max_ms: i64,
    /// 显式代理（如 http://127.0.0.1:7890）。留空 = 直连，
    /// 不跟随系统/环境变量里的代理——那些常常是死的，会让请求全部超时
    proxy: String,
    /// 本地网页预览端口，占用时提示错误
    web_port: u16,
    /// 默认导出目录；留空使用应用配置目录下的 exports
    export_dir: String,
    resource_policy: archive::ResourcePolicy,
    archive_precache: bool,
    archive_worker_threads: Option<usize>,
}

impl Default for Cfg {
    fn default() -> Self {
        Self {
            secret_snapshot: secure::Secrets::default(),
            secrets_blocked: false,
            clear_login: false,
            sessdata: String::new(),
            buvid3: String::new(),
            buvid4: String::new(),
            include_replies: true,
            include_dynamics: true,
            ranges: collection::defaults(),
            interval_ms: 1800,
            interval_max_ms: 2400,
            proxy: String::new(),
            web_port: 8686,
            export_dir: String::new(),
            resource_policy: archive::ResourcePolicy::default(),
            archive_precache: true,
            archive_worker_threads: None,
        }
    }
}

impl Cfg {
    fn archive_workers(&self) -> usize {
        self.archive_worker_threads.unwrap_or_else(local_read::workers)
    }

    fn file() -> Option<PathBuf> {
        checkpoint::data_dir().ok().map(|d| d.join("config.json"))
    }

    fn load() -> Cfg {
        let mut cfg = Cfg::default();
        if let Some(path) = Cfg::file() {
            if let Ok(text) = std::fs::read_to_string(path) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                    match secure::load(&v) {
                        Ok(secrets) => {
                            cfg.sessdata = secrets.sessdata.clone();
                            cfg.buvid3 = secrets.buvid3.clone();
                            cfg.buvid4 = secrets.buvid4.clone();
                            cfg.secret_snapshot = secrets;
                        }
                        Err(_) => {
                            cfg.secrets_blocked = true;
                            report_config_error(
                                "无法读取登录信息，请解锁系统凭据库或重新登录；原凭证已保留",
                            );
                        }
                    }
                    if let Some(b) = v["archive_precache"].as_bool() {
                        cfg.archive_precache = b;
                    }
                    cfg.archive_worker_threads = v["archive_worker_threads"]
                        .as_u64()
                        .and_then(|count| usize::try_from(count).ok())
                        .filter(|count| (1..=local_read::MAX_WORKERS).contains(count));
                    if let Some(b) = v["include_replies"].as_bool() {
                        cfg.include_replies = b;
                    }
                    if let Some(b) = v["include_dynamics"].as_bool() {
                        cfg.include_dynamics = b;
                    }
                    match collection::from_config(&v) {
                        Ok(ranges) => cfg.ranges = ranges,
                        Err(_) => report_config_error("采集范围配置无效，已使用默认范围"),
                    }
                    if let Some(n) = v["interval_ms"].as_i64() {
                        cfg.interval_ms = n.clamp(200, 10000);
                    }
                    // 旧配置没有 interval_max_ms，退回单点值（=不随机），保持原行为
                    if let Some(n) = v["interval_max_ms"].as_i64() {
                        cfg.interval_max_ms = n.clamp(200, 10000);
                    } else if v.get("interval_ms").is_some() {
                        cfg.interval_max_ms = cfg.interval_ms;
                    }
                    if cfg.interval_max_ms < cfg.interval_ms {
                        cfg.interval_max_ms = cfg.interval_ms;
                    }
                    if let Some(s) = v["proxy"].as_str() {
                        cfg.proxy = s.trim().to_string();
                    }
                    if let Some(n) = v["web_port"].as_u64() {
                        if (1024..=65535).contains(&n) {
                            cfg.web_port = n as u16;
                        }
                    }
                    if let Some(s) = v["export_dir"].as_str() {
                        cfg.export_dir = s.to_string();
                    }
                    if let Ok(policy)=serde_json::from_value(v["resource_policy"].clone()){cfg.resource_policy=policy;}

                }
            }
        }
        cfg
    }

    fn save_reported(&mut self) {
        if let Err(e) = self.save() {
            report_config_error(&e.to_string());
        }
    }

    fn save(&mut self) -> Result<()> {
        let path = Cfg::file().ok_or_else(|| anyhow::anyhow!("找不到配置目录，配置未保存"))?;
        self.save_to(&path)
    }

    fn save_to(&mut self, path: &std::path::Path) -> Result<()> {
        let mut v = serde_json::json!({
            "include_replies": self.include_replies,
            "include_dynamics": self.include_dynamics,
            "collection_ranges": self.ranges,
            "interval_ms": self.interval_ms,
            "interval_max_ms": self.interval_max_ms,
            "proxy": self.proxy,
            "web_port": self.web_port,
            "export_dir": self.export_dir,
            "resource_policy": self.resource_policy,
            "archive_precache": self.archive_precache,
            "archive_worker_threads": self.archive_worker_threads,
        });
        let secrets = secure::Secrets {
            sessdata: self.sessdata.clone(),
            buvid3: self.buvid3.clone(),
            buvid4: self.buvid4.clone(),
        };
        let old = match std::fs::read(path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("原配置无效，未覆盖"))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
            Err(_) => anyhow::bail!("原配置无法读取，未覆盖"),
        };
        if self.secrets_blocked && self.sessdata.is_empty() && !self.clear_login {
            secure::preserve(&old, &mut v);
        } else if self.clear_login || self.secrets_blocked || secrets != self.secret_snapshot {
            secure::store(&secrets, &mut v)?;
        } else {
            secure::preserve(&old, &mut v);
        }
        secure::write_config(path, &v)?;
        if !self.secrets_blocked || !self.sessdata.is_empty() || self.clear_login {
            self.secret_snapshot = secrets;
            self.secrets_blocked = false;
            self.clear_login = false;
        }
        Ok(())
    }
}

static CONFIG_ERRORS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
fn report_config_error(message: &str) {
    eprintln!("{}", message);
    let mut errors = CONFIG_ERRORS.lock().unwrap();
    if !errors.iter().any(|e| e == message) {
        errors.push(message.to_string());
    }
}

// ---------------- 共享状态 ----------------
struct Shared {
    doc: Arc<Mutex<ArchiveDoc>>,
    messages: Arc<Mutex<messages::Store>>,
    range_session: Mutex<range_ui::Session>,
    range_batch: std::sync::atomic::AtomicU64,
    message_active: AtomicBool,
    offline: AtomicBool,
    message_view_pending: std::sync::atomic::AtomicU64,
    active_task: AtomicUsize,
    rows: Mutex<Vec<(Video, bool)>>,
    cancel: AtomicBool,
    /// 停止意图：true=暂停（保留断点，下次「继续采集」从这里接上）；false=结束（清断点）
    pause_requested: AtomicBool,
    busy: AtomicBool,
    loading_local: AtomicBool,
    /// 扫码登录的「代次」：每次开新码 +1，旧线程发现代次变了就自行退出
    qr_gen: AtomicUsize,
    cfg: Mutex<Cfg>,
    account_uid: std::sync::atomic::AtomicU64,
    public_account: Arc<Mutex<account_ui::PublicAccount>>,
    checkpoint: Mutex<Option<checkpoint::Task>>,
    log: Mutex<String>,
    last_export: Mutex<Option<PathBuf>>,
}

impl Shared {
    fn new(cfg: Cfg) -> Self {
        let range_session = Mutex::new(range_ui::Session::new(&cfg.ranges));
        Self { range_session, range_batch: std::sync::atomic::AtomicU64::new(0),
            doc: Arc::new(Mutex::new(ArchiveDoc::default())),
            messages: Arc::new(Mutex::new(messages::Store::default())),
            message_active: AtomicBool::new(false),
            offline: AtomicBool::new(false),
            message_view_pending: std::sync::atomic::AtomicU64::new(0),
            active_task: AtomicUsize::new(0),
            public_account: Arc::new(Mutex::new(account_ui::PublicAccount::default())),
            rows: Mutex::new(Vec::new()),
            cancel: AtomicBool::new(false),
            pause_requested: AtomicBool::new(false),
            busy: AtomicBool::new(false),
            loading_local: AtomicBool::new(false),
            qr_gen: AtomicUsize::new(0),
            cfg: Mutex::new(cfg),
            account_uid: std::sync::atomic::AtomicU64::new(0),
            checkpoint: Mutex::new(None),
            log: Mutex::new(String::new()),
            last_export: Mutex::new(None),
        }
    }
}

/// 命令行路径：把配置里存过的设备指纹喂给客户端，避免每次抓取都换一个新指纹
fn apply_saved_fingerprint(client: &mut bili::Bili) {
    let cfg = Cfg::load();
    if !cfg.buvid3.is_empty() {
        client.set_fingerprint(&cfg.buvid3, &cfg.buvid4);
    }
}

/// 建客户端：Cookie + 设备指纹（首次运行会自动取一份并记进配置）
fn make_client(shared: &Arc<Shared>) -> Result<bili::Bili> {
    anyhow::ensure!(!shared.offline.load(Ordering::SeqCst),"离线账号不能调用账号采集接口");
    let (cookie, b3, b4, interval_min, interval_max) = {
        let c = shared.cfg.lock().unwrap();
        (
            c.sessdata.clone(),
            c.buvid3.clone(),
            c.buvid4.clone(),
            c.interval_ms,
            c.interval_max_ms,
        )
    };
    let mut client = bili::Bili::new(cookie)?;
    // 所有翻页拉取共用同一个可配置间隔区间（读取稿件/评论/动态原文/楼中楼…）
    client.interval_min_ms = interval_min.clamp(200, 10000) as u64;
    client.interval_max_ms = interval_max.clamp(200, 10000) as u64;
    if b3.is_empty() {
        let (n3, n4) = client.fingerprint();
        let (n3, n4) = (n3.to_string(), n4.to_string());
        if !n3.is_empty() {
            let mut cfg = shared.cfg.lock().unwrap();
            cfg.buvid3 = n3;
            cfg.buvid4 = n4;
            cfg.save_reported();
        }
    } else {
        client.set_fingerprint(&b3, &b4);
    }
    Ok(client)
}

// ---------------- 后台命令 ----------------
enum Cmd {
    ArchiveModeChanged,
    Offline(offline::Action),
    EnsureMessageView {uid:u64},
    Messages(messages_ui::Action),
    ReadArchives { cookie: String, batch: u64 },
    AddVideos(Vec<String>),
    Start,
    CrawlUp,
    QueryTotals { types: Vec<u32>, uid: u64, cookie: String, batch: u64 },
    ProcessResources,
    Export { path: PathBuf },
}

// ---------------- 小工具 ----------------
fn emit(ui: &Weak<AppWindow>, f: impl FnOnce(&AppWindow) + Send + 'static) {
    let w = ui.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(win) = w.upgrade() {
            f(&win);
        }
    });
}

fn push_log(shared: &Arc<Shared>, ui: &Weak<AppWindow>, msg: impl AsRef<str>) {
    let line = format!(
        "{}  {}",
        chrono::Local::now().format("%H:%M:%S"),
        msg.as_ref()
    );
    let text = {
        let mut log = shared.log.lock().unwrap();
        let mut lines: Vec<&str> = log.lines().collect();
        // 日志最新在上
        let mut all = vec![line.as_str()];
        all.extend(lines.iter().copied());
        all.truncate(MAX_LOG_LINES);
        lines.clear();
        let joined = all.join("\n");
        *log = joined.clone();
        joined
    };
    emit(ui, move |w| w.set_log_text(text.into()));
}

// 0 = no cancellable task, 1 = comments, 2 = messages, 3 = resources.
fn set_active_task(shared:&Arc<Shared>,ui:&Weak<AppWindow>,task_type:usize) {
    shared.active_task.store(task_type,Ordering::SeqCst);emit(ui,move|w|w.set_active_task(task_type as i32));
}
struct TaskGuard {shared:Arc<Shared>,ui:Weak<AppWindow>}
impl TaskGuard {
    fn begin(shared:&Arc<Shared>,ui:&Weak<AppWindow>,task_type:usize)->Option<Self> {
        if task_type==0 {return None;}
        if shared.active_task.load(Ordering::SeqCst)==0 {shared.cancel.store(false,Ordering::SeqCst);shared.pause_requested.store(false,Ordering::SeqCst);}
        set_active_task(shared,ui,task_type);set_busy(shared,ui,true);
        Some(Self{shared:Arc::clone(shared),ui:ui.clone()})
    }
}
impl Drop for TaskGuard {
    fn drop(&mut self) {set_active_task(&self.shared,&self.ui,0);set_busy(&self.shared,&self.ui,false);}
}
fn set_busy(shared: &Arc<Shared>, ui: &Weak<AppWindow>, busy: bool) {
    shared.busy.store(busy, Ordering::SeqCst);
    emit(ui, move |w| w.set_busy(busy));
}

fn set_progress(ui: &Weak<AppWindow>, ratio: f32, text: String) {
    emit(ui, move |w| {
        w.set_progress(ratio.clamp(0.0, 1.0));
        w.set_progress_text(text.into());
    });
}

/// 同步断点操作按钮的显示状态。
fn refresh_resume(shared: &Arc<Shared>, ui: &Weak<AppWindow>) {
    let has = shared.checkpoint.lock().unwrap().is_some();
    emit(ui, move |w| w.set_has_resume(has));
}

fn set_status(ui: &Weak<AppWindow>, text: String, ok: bool) {
    emit(ui, move |w| {
        w.set_status_text(text.into());
        w.set_status_ok(ok);
    });
}

fn set_foot(ui: &Weak<AppWindow>, text: String) {
    emit(ui, move |w| w.set_foot_text(text.into()));
}

fn log_foot(shared: &Arc<Shared>, ui: &Weak<AppWindow>, text: impl Into<String>) {
    let text = text.into();
    push_log(shared, ui, &text);
    set_foot(ui, text);
}

/// 风控弹窗：显示原因与倒计时
fn verify_show(ui: &Weak<AppWindow>, secs: u64, text: String) {
    emit(ui, move |w| {
        w.set_verify_text(text.into());
        w.set_verify_waiting(true);
        w.set_verify_secs(secs as i32);
        if !w.get_verify_background() { w.set_verify_visible(true); }
    });
}

fn verify_tick(ui: &Weak<AppWindow>, secs: u64) {
    emit(ui, move |w| w.set_verify_secs(secs as i32));
}

fn verify_hide(ui: &Weak<AppWindow>) {
    emit(ui, |w| { w.set_verify_visible(false); w.set_verify_background(false); w.set_verify_waiting(false); w.set_verify_secs(0); });
}

// ---------------- 扫码登录 ----------------
/// 二维码图只在 UI 线程里构造：slint::Image 不能跨线程传，传像素过去就行
fn qr_show(ui: &Weak<AppWindow>, text: &str, status: &str) {
    match qrlogin::qr_rgba(text, 480) {
        Some((pixels, w, h)) => {
            let status = status.to_string();
            emit(ui, move |win| {
                let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(w, h);
                buf.make_mut_bytes().copy_from_slice(&pixels);
                win.set_qr_image(slint::Image::from_rgba8(buf));
                win.set_qr_empty(false);
                win.set_qr_status(status.into());
            });
        }
        None => emit(ui, |win| {
            win.set_qr_empty(true);
            win.set_qr_status("二维码生成失败，请点「换个二维码」".into());
        }),
    }
}

fn qr_status(ui: &Weak<AppWindow>, text: String) {
    emit(ui, move |w| w.set_qr_status(text.into()));
}

/// 后台线程：拿码 → 轮询 → 成功就把整套 Cookie 存下来并自动拉稿件
fn run_qr_login(
    shared: Arc<Shared>,
    ui: Weak<AppWindow>,
    tx: std::sync::mpsc::Sender<Cmd>,
    my_gen: usize,
) {
    let alive = || shared.qr_gen.load(Ordering::SeqCst) == my_gen;
    let login = match qrlogin::QrLogin::new() {
        Ok(l) => l,
        Err(e) => {
            qr_status(&ui, format!("初始化失败：{}", e));
            return;
        }
    };
    let mut key;

    loop {
        if !alive() {
            return;
        }
        match login.generate() {
            Ok((url, k)) => {
                key = k;
                qr_show(&ui, &url, "等待扫码…请用 B 站手机客户端扫一扫");
            }
            Err(e) => {
                qr_status(&ui, format!("获取二维码失败：{}", e));
                return;
            }
        }

        // 一张码轮询到过期或被取消
        let mut last = qrlogin::status_text(&qrlogin::Poll::Waiting).to_string();
        loop {
            if !alive() {
                return;
            }
            std::thread::sleep(qrlogin::POLL_INTERVAL);
            if !alive() {
                return;
            }
            match login.poll(&key) {
                Ok(poll) => {
                    if !alive() {
                        return;
                    }
                    let text = qrlogin::status_text(&poll).to_string();
                    match poll {
                        qrlogin::Poll::Waiting | qrlogin::Poll::Scanned => {
                            if text != last {
                                last = text.clone();
                                qr_status(&ui, text);
                            }
                        }
                        qrlogin::Poll::Expired => {
                            qr_status(&ui, text);
                            break;
                        }
                        qrlogin::Poll::Done { cookie, mid } => {
                            {
                                let mut cfg = shared.cfg.lock().unwrap();
                                cfg.sessdata = cookie.clone();
                                if !login.buvid3.is_empty() && cfg.buvid3.is_empty() {
                                    cfg.buvid3 = login.buvid3.clone();
                                    cfg.buvid4 = login.buvid4.clone();
                                }
                                cfg.save_reported();
                            }
                            enqueue_archive_read(&shared,&ui,&tx);
                            let cookie_ui = cookie.clone();
                            emit(&ui, move |w| {
                                w.set_qr_visible(false);
                                w.set_sessdata(cookie_ui.into());
                                w.set_account_text(format!("扫码登录成功（UID {}）", mid).into());
                            });
                            set_status(&ui, "已连接".into(), true);
                            push_log(&shared, &ui, format!("扫码登录成功，UID {}", mid));
                            set_foot(
                                &ui,
                                if login.buvid3.is_empty() {
                                    "登录成功，正在读取稿件…".to_string()
                                } else {
                                    "登录成功（设备指纹已保存），正在读取稿件…".to_string()
                                },
                            );
                            return;
                        }
                    }
                }
                Err(e) => {
                    let text = format!("查询扫码状态失败：{}", e);
                    if text != last {
                        last = text.clone();
                        qr_status(&ui, text);
                    }
                }
            }
        }
    }
}

fn refresh_rows(shared: &Arc<Shared>, ui: &Weak<AppWindow>) {
    let rows = shared.rows.lock().unwrap();
    let list: Vec<VideoRow> = content_rows::sorted_indices(&rows).into_iter().map(|index| {
        let (v,checked)=&rows[index];let identity=content_rows::identity(v);
        VideoRow {
            index:index as i32, bvid:v.bvid.clone().into(),
            title:if v.title.trim().is_empty(){identity.clone()}else{v.title.clone()}.into(),
            meta:if v.comment_total>0 {format!("{} · {} 评论 · {}",identity,human_count(v.comment_total),content_rows::time_label(v))}else{format!("{} · {}",identity,content_rows::time_label(v))}.into(),
            checked:*checked,
        }
    }).collect();
    let selected = rows.iter().filter(|(_, c)| *c).count();
    drop(rows);
    emit(ui, move |w| {
        w.set_videos(ModelRc::new(VecModel::from(list)));
        w.set_selected_count(selected as i32);
    });
}

fn refresh_stats(shared: &Arc<Shared>, ui: &Weak<AppWindow>) {
    let doc = shared.doc.lock().unwrap();
    let stats = doc.stats.clone();
    let has = stats.comments > 0;
    drop(doc);
    emit(ui, move |w| {
        w.set_stat_videos(stats.videos as i32);
        w.set_stat_dynamics(stats.dynamics as i32);
        w.set_stat_comments(stats.comments as i32);
        w.set_stat_replies(stats.replies as i32);
        w.set_stat_likes(human_count(stats.likes).into());
        w.set_has_data(has);
    });
}

fn default_export_name() -> String {
    format!(
        "bili-comments-{}.json",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    )
}

/// 读取现有分类归档所用的根目录。
fn export_dir_of(cfg: &Cfg) -> String {
    if let Some(active)=library::active(){return active.root.to_string_lossy().into_owned();}
    if !cfg.export_dir.trim().is_empty() {
        return cfg.export_dir.trim().to_string();
    }
    checkpoint::data_dir()
        .map(|p| p.join("exports").to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// 在资源管理器里打开路径：文件就定位选中，目录就直接打开
fn open_path(path: &str) {
    let p = PathBuf::from(path);
    if p.is_file() {
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{}", p.display()))
            .spawn();
    } else {
        let _ = std::process::Command::new("explorer").arg(path).spawn();
    }
}

/// 起（或复用）本地网页预览并打开浏览器。cache 里存已启动的地址，
/// 端口是启动那一刻定下的，改端口要重启程序才生效。
/// 网页服务的运行状态：地址用于重复打开浏览器，stop 用于主动停止并释放端口
struct WebHandle {
    url: String,
    process: preview_process::Process,
}

fn web_running(ui: &Weak<AppWindow>, running: bool) {
    emit(ui, move |w| w.set_web_running(running));
}

fn preview_url(base: &str, workspace: i32) -> String {
    format!("{}/{}", base.trim_end_matches('/'), if workspace == 1 { "messages" } else { "comments" })
}

#[cfg(test)]
mod preview_route_tests {
    #[test] fn current_workspace_determines_preview_path() {
        assert_eq!(super::preview_url("http://127.0.0.1:8686/",0),"http://127.0.0.1:8686/comments");
        assert_eq!(super::preview_url("http://127.0.0.1:8686/",1),"http://127.0.0.1:8686/messages");
        assert_eq!(super::preview_url("http://127.0.0.1:9000",1),"http://127.0.0.1:9000/messages");
    }
}

/// 服务基础地址与工作区路由分离，打开时读取当前工作区。
fn ensure_web(
    shared: &Arc<Shared>,
    ui: &Weak<AppWindow>,
    cache: &Arc<Mutex<Option<WebHandle>>>,
    port: u16,
    open_browser: bool,
) {
    let workspace = ui.upgrade().map(|w| w.get_workspace()).unwrap_or(0);
    let existing = {
        let mut slot = cache.lock().unwrap();
        if slot.as_ref().is_some_and(|h| !h.process.running()) { slot.take(); }
        slot.as_ref().map(|h| h.url.clone())
    };
    if let Some(url) = existing {
        if open_browser {
            let _ = webbrowser::open(&preview_url(&url, workspace));
        }
        return;
    }
    let exited_shared = Arc::downgrade(shared);
    let exited_cache = Arc::downgrade(cache);
    let exited_ui = ui.clone();
    match preview_process::Process::start(shared, port, move || {
        let _ = slint::invoke_from_event_loop(move || {
            if let (Some(cache), Some(shared)) = (exited_cache.upgrade(), exited_shared.upgrade()) {
                let mut slot = cache.lock().unwrap();
                if slot.as_ref().is_some_and(|h| !h.process.running()) {
                    slot.take();
                    drop(slot);
                    let message = "网页预览进程意外退出";
                    push_log(&shared, &exited_ui, message);
                    set_foot(&exited_ui, message.into());
                    web_running(&exited_ui, false);
                }
            }
        });
    }) {
        Ok((port, process)) => {
            let url = format!("http://127.0.0.1:{}/", port);
            *cache.lock().unwrap() = Some(WebHandle { url: url.clone(), process });
            push_log(
                shared,
                ui,
                format!("网页服务已启动：{}", url),
            );
            set_status(ui, "网页预览已启动".into(), true);
            set_foot(ui, format!("网页服务已启动：{}", url));
            let address = url.clone();
            emit(ui, move |w| w.set_web_address(address.into()));
            web_running(ui, true);
            if open_browser {
                let _ = webbrowser::open(&preview_url(&url, workspace));
            }
        }
        Err(e) => {
            port_alert(ui, e.to_string());
            push_log(shared, ui, format!("网页启动失败：{}", e));
            set_foot(ui, "网页启动失败".into());
            web_running(ui, false);
        }
    }
}

/// 停止服务：置位后监听线程自行退出，端口随之释放，下次启动仍可用同一端口
fn stop_web(shared: &Arc<Shared>, ui: &Weak<AppWindow>, cache: &Arc<Mutex<Option<WebHandle>>>) {
    let handle = cache.lock().unwrap().take();
    match handle {
        Some(h) => {
            let url = h.url.clone();
            drop(h);
            push_log(shared, ui, format!("网页服务已停止：{}", url));
            set_status(ui, "网页服务已停止".into(), false);
            web_running(ui, false);
        }
        None => push_log(shared, ui, "网页服务本来就没在运行"),
    }
}

#[derive(Debug)]
struct CommentArchiveFailure;
impl std::fmt::Display for CommentArchiveFailure {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {write!(f,"评论自动归档或断点写入失败，已停止采集")}
}
impl std::error::Error for CommentArchiveFailure {}
fn persist_comment_progress(root:&std::path::Path,task:&checkpoint::Task,videos:&[Video])->Result<usize> {
    export::persist_comments(root,task.uid,videos,||task.save()).context(CommentArchiveFailure)
}
fn comment_archive_failure(shared:&Arc<Shared>,ui:&Weak<AppWindow>,task:&checkpoint::Task,error:impl std::fmt::Display) {
    active_task(shared,ui,task);
    let text=format!("评论自动归档失败，断点已保留：{}",error);
    set_status(ui,text.clone(),false);set_progress(ui,0.0,text.clone());log_foot(shared,ui,text);set_busy(shared,ui,false);
}

/// Optional offline resources are independent of manual JSON export.
fn process_comment_resources(shared:&Arc<Shared>,ui:&Weak<AppWindow>,videos:&[Video],dir:&std::path::Path) {
    if shared.cancel.load(Ordering::SeqCst) {return;}
    let (policy,low,high)={let cfg=shared.cfg.lock().unwrap();(cfg.resource_policy.clone(),cfg.interval_ms.max(0)as u64,cfg.interval_max_ms.max(0)as u64)};
    if policy.avatars||policy.covers||policy.content_images {
        set_active_task(shared,ui,3);set_busy(shared,ui,true);
        let mut snapshot=ArchiveDoc::default();snapshot.videos=videos.to_vec();let uid=shared.account_uid.load(Ordering::SeqCst);let root=archive::root(dir);let mut resources=archive::from_comments(&snapshot,uid,&root,policy);
        if let Err(error)=archive::process_resources(&mut resources,&root,&shared.cancel,|url,path|{messages::pause(Duration::from_millis(bili::random_delay_ms(low,high)),&shared.cancel)?;messages::download(url,path,&shared.cancel)},|n,total,saved,failed,_|set_progress(ui,if total==0{1.0}else{n as f32/total as f32},format!("资源 {n}/{total} · 已保存 {saved} / 失败 {failed}"))){push_log(shared,ui,error.to_string());}
    }
}

/// Each queued request owns only its loading flags, even across account resets.
struct RangeReadGuard {shared:Arc<Shared>,ui:Weak<AppWindow>,batch:u64}
impl RangeReadGuard {
    fn new(shared:&Arc<Shared>,ui:&Weak<AppWindow>,batch:u64)->Self {Self{shared:shared.clone(),ui:ui.clone(),batch}}
}
impl Drop for RangeReadGuard {
    fn drop(&mut self){
        self.shared.range_session.lock().unwrap().finish(self.batch);
        render_range_session(&self.shared,&self.ui);
    }
}
fn begin_range_read(shared:&Arc<Shared>,ui:&Weak<AppWindow>,types:&[u32])->u64 {
    let batch=shared.range_batch.fetch_add(1,Ordering::SeqCst).wrapping_add(1);
    shared.range_session.lock().unwrap().begin(batch,types);
    render_range_session(shared,ui);batch
}
fn enqueue_archive_read(shared:&Arc<Shared>,ui:&Weak<AppWindow>,tx:&std::sync::mpsc::Sender<Cmd>)->bool {
    if shared.offline.load(Ordering::SeqCst){return false;}
    let cookie=shared.cfg.lock().unwrap().sessdata.clone();
    if cookie.trim().is_empty(){return false;}
    let batch=begin_range_read(shared,ui,&collection::TYPES);
    if tx.send(Cmd::ReadArchives{cookie,batch}).is_err(){
        shared.range_session.lock().unwrap().finish(batch);render_range_session(shared,ui);return false;
    }
    true
}

fn compact_archives(shared: &Shared) {
    let (precache, root) = {
        let cfg = shared.cfg.lock().unwrap();
        (cfg.archive_precache, PathBuf::from(export_dir_of(&cfg)))
    };
    let uid = shared.account_uid.load(Ordering::SeqCst);
    let guest_file = uid == 0 && !shared.offline.load(Ordering::SeqCst) && preload_path().is_some();
    let has_comment_bodies = shared.doc.lock().unwrap().videos.iter().any(|v| !v.comments.is_empty());
    if !guest_file && has_comment_bodies && (!precache || !preview_disk::small_comment_archive(&root, uid)) {
        let mut archive = shared.doc.lock().unwrap();
        for video in &mut archive.videos { video.comments = Vec::new(); }
        if let Some(resources) = archive.archive.get_mut("resources") {
            *resources = serde_json::Value::Null;
        }
    }
    if let Some(task) = shared.checkpoint.lock().unwrap().as_mut() {
        task.rows = Vec::new();
        task.cursors.clear();
        for target in &mut task.targets { target.comments = Vec::new(); }
    }
    let mut messages = shared.messages.lock().unwrap();
    messages.doc.items = Vec::new();
    messages.doc.sessions = Vec::new();
    messages.doc.archive = serde_json::Value::Null;
}

struct ReleaseArchives<'a>(&'a Shared);
impl Drop for ReleaseArchives<'_> {
    fn drop(&mut self) { compact_archives(self.0); }
}

fn reload_comments(shared: &Shared) {
    let cfg = shared.cfg.lock().unwrap();
    let uid = shared.account_uid.load(Ordering::SeqCst);
    let root = PathBuf::from(export_dir_of(&cfg));
    drop(cfg);
    if uid == 0 && preload_path().is_some() && !shared.offline.load(Ordering::SeqCst) { return; }
    let mut loaded = preview_disk::read_comments(&root, uid);
    let mut doc = shared.doc.lock().unwrap();
    loaded.merge_videos(std::mem::take(&mut doc.videos));
    *doc = loaded;
}

fn worker(rx: Receiver<Cmd>, shared: Arc<Shared>, ui: Weak<AppWindow>) {
    while let Ok(cmd) = rx.recv() {
        let _release = ReleaseArchives(&shared);
        let cmd=match cmd {Cmd::Offline(action)=>{offline::run(action,&shared,&ui);continue;},other=>other};
        if let Cmd::EnsureMessageView{uid}=&cmd {
            messages_ui::ensure_view(*uid,&shared,&ui);
            continue;
        }
        let task_type=match &cmd {Cmd::Start|Cmd::CrawlUp=>1,Cmd::ProcessResources=>3,_=>0};
        let _task=TaskGuard::begin(&shared,&ui,task_type);
        let on_demand = !shared.cfg.lock().unwrap().archive_precache;
        if matches!(&cmd, Cmd::Start | Cmd::CrawlUp | Cmd::ProcessResources | Cmd::Export { .. }) {
            let compact = {
                let doc = shared.doc.lock().unwrap();
                doc.stats.comments > 0 && doc.videos.iter().all(|v| v.comments.is_empty())
            };
            if on_demand || compact { reload_comments(&shared); }
        }
        match cmd {
            Cmd::ArchiveModeChanged => {
                if !on_demand {
                    let _reading = LocalReadGuard::begin(&shared, &ui);
                    let uid = shared.account_uid.load(Ordering::SeqCst);
                    let root = PathBuf::from(export_dir_of(&shared.cfg.lock().unwrap()));
                    if (uid != 0 || shared.offline.load(Ordering::SeqCst))
                        && preview_disk::small_comment_archive(&root, uid)
                    {
                        reload_comments(&shared);
                        refresh_stats(&shared, &ui);
                    }
                    if uid != 0 { messages_ui::load_overview(&shared, &ui, uid); }
                }
            }
            Cmd::Offline(_)=>unreachable!("offline actions are handled separately"),
            Cmd::EnsureMessageView{..} => unreachable!("view requests are handled before task state changes"),
            Cmd::Messages(action) => messages_ui::run(action, &shared, &ui),
            Cmd::ReadArchives {cookie,batch} => {
                let _range_loading=RangeReadGuard::new(&shared,&ui,batch);
                if shared.cfg.lock().unwrap().sessdata!=cookie {continue;}
                let expected_cookie = cookie.clone();
                if cookie.trim().is_empty() {
                    push_log(&shared, &ui, "请先扫码登录，再试");
                    set_foot(&ui, "尚未登录".into());
                    continue;
                }
                set_busy(&shared, &ui, true);
                set_progress(&ui, 0.05, "正在校验账号…".into());
                let client = match make_client(&shared) {
                    Ok(c) => c,
                    Err(e) => {
                        push_log(&shared, &ui, format!("初始化失败：{}", e));
                        set_busy(&shared, &ui, false);
                        continue;
                    }
                };
                match client.login_state() {
                    Ok(profile) if profile.logged_in && profile.uid > 0 => {
                        let mid = profile.uid;
                        let uname = profile.name.clone();
                        if shared.cfg.lock().unwrap().sessdata != expected_cookie {
                            set_busy(&shared, &ui, false);
                            continue;
                        }
                        set_account_with_loading(&shared, &ui, mid, true);
                        set_status(&ui, format!("已连接 · {}", uname), true);
                        let who = uname.clone();
                        account_ui::publish(&shared, &ui, profile, expected_cookie.clone());
                        push_log(&shared, &ui, format!("账号校验通过：{}", who));
                    }
                    Ok(_) => {
                        if shared.cfg.lock().unwrap().sessdata != expected_cookie {
                            set_busy(&shared, &ui, false);
                            continue;
                        }
                        set_account(&shared, &ui, 0);
                        set_status(&ui, "Cookie 无效".into(), false);
                        push_log(&shared, &ui, "SESSDATA 未登录或已过期");
                        set_busy(&shared, &ui, false);
                        continue;
                    }
                    Err(e) => {
                        set_status(&ui, "账号校验失败".into(), false);
                        push_log(&shared, &ui, format!("校验失败：{}", e));
                        set_busy(&shared, &ui, false);
                        continue;
                    }
                }

                let uid = shared.account_uid.load(Ordering::SeqCst);
                let (all, total) = std::thread::scope(|scope| {
                    let local = scope.spawn(|| {
                        load_startup_archives(&shared, &ui, uid, &expected_cookie);
                    });
                    set_progress(&ui, 0.1, "正在读取线上稿件…".into());
                    let mut all: Vec<Video> = Vec::new();
                    let mut pn = 1u64;
                    let ps = 50u64;
                    let mut total = 0u64;
                    loop {
                        match client.archives_page(pn, ps) {
                            Ok((list, t)) => {
                                total = t;
                                let n = list.len();
                                all.extend(list);
                                set_progress(
                                    &ui,
                                    if total > 0 {
                                        (all.len() as f32 / total as f32).min(1.0)
                                    } else {
                                        0.5
                                    },
                                    format!("稿件 {}/{}", all.len(), total),
                                );
                                if n == 0 || all.len() as u64 >= total || pn >= 12 {
                                    break;
                                }
                                pn += 1;
                                std::thread::sleep(client.next_pause());
                            }
                            Err(e) => {
                                push_log(&shared, &ui, format!("读取稿件失败：{}", e));
                                break;
                            }
                        }
                    }
                    local.join().expect("startup archive reader panicked");
                    (all, total)
                });

                if all.is_empty() {
                    push_log(&shared, &ui, "没有读到稿件，请检查 SESSDATA 或稍后重试");
                    push_log(&shared, &ui, "已保留本地历史数据与断点，线上稿件可稍后重试");
                }

                if shared.cfg.lock().unwrap().sessdata != expected_cookie {
                    set_busy(&shared, &ui, false);
                    continue;
                }
                {
                    let mut rows = shared.rows.lock().unwrap();
                    for v in all {
                        merge_online_metadata(&shared, &v);
                        if let Some(pos) = rows
                            .iter()
                            .position(|(r, _)| checkpoint::key(r) == checkpoint::key(&v))
                        {
                            rows[pos].0 = v;
                        } else {
                            rows.push((v, false));
                        }
                    }
                }
                sync_history_rows(&shared);
                refresh_stats(&shared, &ui);
                refresh_rows(&shared, &ui);
                set_progress(&ui, 1.0, format!("稿件 {}", total));
                if shared.checkpoint.lock().unwrap().is_none() {
                    set_foot(&ui, "稿件已载入，勾选后开始采集".into());
                }

                query_type_totals(&shared, &ui, &client, &collection::TYPES, &expected_cookie, uid, batch);
                set_busy(&shared, &ui, false);
            }

            Cmd::QueryTotals { types, uid, cookie: expected, batch } => {
                let _range_loading=RangeReadGuard::new(&shared,&ui,batch);
                if uid == 0 || expected.is_empty() || shared.account_uid.load(Ordering::SeqCst) != uid || shared.cfg.lock().unwrap().sessdata != expected {
                    continue;
                }
                match make_client(&shared) {
                    Ok(client) => query_type_totals(&shared, &ui, &client, &types, &expected, uid, batch),
                    Err(e) => {
                        push_log(&shared,&ui,format!("获取类型总数失败：{}",e));
                    },
                }
            }
            Cmd::ProcessResources => {
                set_busy(&shared,&ui,true);shared.message_active.store(true,Ordering::SeqCst);emit(&ui,|w|w.set_message_active(true));
                let (root,policy,low,high)={let cfg=shared.cfg.lock().unwrap();(archive::root(std::path::Path::new(&export_dir_of(&cfg))),cfg.resource_policy.clone(),cfg.interval_ms.max(0)as u64,cfg.interval_max_ms.max(0)as u64)};
                let uid=shared.account_uid.load(Ordering::SeqCst);
                let snapshot=shared.doc.lock().unwrap().clone();let mut document=archive::from_comments(&snapshot,uid,&root,policy);
                let result=archive::process_resources(&mut document,&root,&shared.cancel,|url,path|{messages::pause(Duration::from_millis(bili::random_delay_ms(low,high)),&shared.cancel)?;messages::download(url,path,&shared.cancel)},|n,total,saved,failed,_|set_progress(&ui,if total==0{1.0}else{n as f32/total as f32},format!("资源 {n}/{total} · 已保存 {saved} / 失败 {failed}")));
                set_busy(&shared,&ui,false);
                shared.message_active.store(false,Ordering::SeqCst);emit(&ui,|w|w.set_message_active(false));
                let cancelled = shared.cancel.load(Ordering::SeqCst);
                match result {
                    Err(error) => log_foot(&shared, &ui, format!("评论资源处理失败：{error:#}")),
                    Ok(()) if cancelled => log_foot(&shared, &ui, "资源处理已暂停或结束，已保存进度保留"),
                    Ok(()) if uid == 0 => log_foot(&shared, &ui, "评论资源处理完成；登录后可处理账号消息资源"),
                    Ok(()) => {}
                }
                if uid > 0 && !cancelled {
                    messages_ui::run(messages_ui::Action::Attachments(uid), &shared, &ui);
                }
            }
            Cmd::AddVideos(items) => {
                set_busy(&shared, &ui, true);
                let client = match make_client(&shared) {
                    Ok(c) => c,
                    Err(_) => {
                        set_busy(&shared, &ui, false);
                        continue;
                    }
                };
                let mut added = 0;
                for (i, item) in items.iter().enumerate() {
                    set_progress(
                        &ui,
                        (i + 1) as f32 / items.len() as f32,
                        format!("解析 {}/{}", i + 1, items.len()),
                    );
                    match client.video_info(item) {
                        Ok(v) => {
                            let mut rows = shared.rows.lock().unwrap();
                            if let Some(pos) = rows.iter().position(|(r, _)| r.aid == v.aid) {
                                rows[pos].0 = v;
                                rows[pos].1 = true;
                            } else {
                                push_log(&shared, &ui, format!("加入：{}（av{}）", v.title, v.aid));
                                rows.push((v, true));
                                added += 1;
                            }
                        }
                        Err(e) => {
                            push_log(&shared, &ui, format!("{}：{}", item, e));
                        }
                    }
                    std::thread::sleep(Duration::from_millis(150));
                }
                refresh_rows(&shared, &ui);
                if added > 0 {
                    log_foot(&shared, &ui, format!("已加入 {} 个稿件", added));
                }
                set_busy(&shared, &ui, false);
            }

            Cmd::Start => {
                let mut targets: Vec<Video> = shared
                    .rows
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(_, c)| *c)
                    .map(|(v, _)| v.clone())
                    .collect();
                if targets.is_empty() {
                    push_log(&shared, &ui, "请先勾选稿件");
                    continue;
                }
                {
                    let doc = shared.doc.lock().unwrap();
                    let saved: HashMap<_, _> = doc.videos.iter()
                        .map(|video| ((video.reply_type(), video.aid), &video.comments))
                        .collect();
                    for target in &mut targets {
                        if target.comments.is_empty() {
                            if let Some(comments) = saved.get(&(target.reply_type(), target.aid)) {
                                target.comments = (**comments).clone();
                            }
                        }
                    }
                }
                let cfg = shared.cfg.lock().unwrap().clone();
                let output_root=PathBuf::from(export_dir_of(&cfg));
                let mut client = match make_client(&shared) {
                    Ok(c) => c,
                    Err(e) => {
                        push_log(&shared, &ui, e.to_string());
                        continue;
                    }
                };
                let (uid, _) = client.me();
                if uid == 0 {
                    push_log(&shared, &ui, "请先登录后再采集");
                    continue;
                }
                set_account(&shared, &ui, uid);
                let mut task = match checkpoint::Task::new(
                    uid,
                    cfg.ranges.clone(),
                    cfg.include_replies,
                    targets,
                )
                .resume()
                {
                    Ok(t) => t,
                    Err(e) => {
                        push_log(&shared, &ui, format!("读取进度失败，未覆盖原文件：{}", e));
                        continue;
                    }
                };
                if let Err(error)=persist_comment_progress(&output_root,&task,&task.doc().videos) {
                    comment_archive_failure(&shared,&ui,&task,format!("{error:#}"));continue;
                }
                push_log(&shared,&ui,format!("评论自动归档目录：{}",archive::root(&output_root).join(task.uid.to_string()).join("comments").display()));
                {
                    let mut rows = shared.rows.lock().unwrap();
                    for target in &task.targets {
                        if !rows
                            .iter()
                            .any(|(v, _)| checkpoint::key(v) == checkpoint::key(target))
                        {
                            rows.push((target.without_comments(), true));
                        }
                    }
                }
                active_task(&shared, &ui, &task);

                set_busy(&shared, &ui, true);
                let mut failure = None;
                for index in 0..task.targets.len() {
                    if shared.cancel.load(Ordering::SeqCst) {
                        break;
                    }
                    let mut video = task.targets[index].clone();
                    let key = checkpoint::key(&video);
                    if task.done.contains(&key) {
                        continue;
                    }
                    let mut cursor = task.cursors.get(&key).cloned().unwrap_or_default();
                    log_foot(&shared, &ui, format!("采集：{}", video.display_title()));
                    let result = client.comments_of_resume(
                        &mut video,
                        cfg.include_replies,
                        &shared.cancel,
                        &mut cursor,
                        |v, cursor| {
                            if shared.cfg.lock().unwrap().sessdata != cfg.sessdata {
                                anyhow::bail!("登录账号已变更，原任务已保留");
                            }
                            let mut next=task.clone();
                            next.targets[index]=v.clone();next.cursors.insert(key.clone(),cursor.clone());
                            persist_comment_progress(&output_root,&next,std::slice::from_ref(v))?;
                            task=next;
                            active_task(&shared, &ui, &task);
                            {
                                shared.doc.lock().unwrap().merge_video(v.clone());
                            }
                            let progress = format!(
                                "稿件 {}/{} · 已保存 {} 条主评论 / {} 条回复",
                                index + 1, task.targets.len(), v.comments.len(),
                                v.comments.iter().map(|comment| comment.replies.len()).sum::<usize>()
                            );
                            set_progress(&ui, index as f32 / task.targets.len().max(1) as f32, progress.clone());
                            log_foot(&shared, &ui, progress);
                            Ok(())
                        },
                    );
                    shared.doc.lock().unwrap().merge_video(video.clone());
                    match result {
                        Ok(())=>{
                            let mut next=task.clone();next.targets[index]=video.clone();next.cursors.insert(key.clone(),cursor);next.done.push(key);
                            if let Err(error)=persist_comment_progress(&output_root,&next,std::slice::from_ref(&video)) {
                                failure = Some(error);
                                break;
                            }
                            task=next;
                            log_foot(&shared, &ui, format!("已保存稿件 {}/{} · {}",
                                task.done.len(), task.targets.len(), video.display_title()));
                        },
                        Err(error)=>{
                            if error.is::<CommentArchiveFailure>() || !shared.cancel.load(Ordering::SeqCst) {
                                failure = Some(error);
                            }
                            break;
                        }
                    }
                    refresh_stats(&shared, &ui);
                }
                let stopped = failure.is_none() && shared.cancel.load(Ordering::SeqCst)
                    && !shared.pause_requested.load(Ordering::SeqCst);
                let complete = failure.is_none() && task.complete();
                if complete || stopped {
                    clear_checkpoint(&shared, &ui);
                } else {
                    active_task(&shared, &ui, &task);
                }
                let message = if let Some(error) = &failure {
                    format!("采集失败，进度已保留：{error:#}")
                } else if complete {
                    "采集完成".to_owned()
                } else if stopped {
                    "已结束".to_owned()
                } else {
                    format!("已暂停 · {}", task.summary())
                };
                set_busy(&shared, &ui, false);
                refresh_stats(&shared, &ui);
                set_progress(&ui, if complete { 1.0 } else { 0.0 }, message.clone());
                log_foot(&shared, &ui, message);
                if failure.is_none() {process_comment_resources(&shared, &ui, &task.targets,&output_root);}
            }

            Cmd::CrawlUp => {
                let cfg = shared.cfg.lock().unwrap().clone();
                let output_root=PathBuf::from(export_dir_of(&cfg));
                if cfg.sessdata.trim().is_empty() {
                    push_log(&shared, &ui, "全量模式需要登录：请先扫码登录");
                    set_foot(&ui, "尚未登录".into());
                    continue;
                }
                let mut client = match make_client(&shared) {
                    Ok(c) => c,
                    Err(e) => {
                        push_log(&shared, &ui, format!("初始化失败：{}", e));
                        continue;
                    }
                };
                let (self_mid, self_name) = client.me();
                if self_mid == 0 {
                    set_status(&ui, "Cookie 无效".into(), false);
                    push_log(&shared, &ui, "SESSDATA 无效或已过期，无法读取全量评论");
                    set_foot(&ui, "请更新 SESSDATA".into());
                    continue;
                }
                set_account(&shared, &ui, self_mid);
                set_status(&ui, format!("已连接 · {}", self_name), true);
                {
                    let name = self_name.clone();
                    emit(&ui, move |w| {
                        w.set_account_text(format!("已登录：{}（UID {}）", name, self_mid).into())
                    });
                }

                set_busy(&shared, &ui, true);

                let mut task = match checkpoint::Task::new(
                    self_mid,
                    cfg.ranges.clone(),
                    cfg.include_replies,
                    vec![],
                )
                .resume()
                {
                    Ok(t) => t,
                    Err(e) => {
                        push_log(&shared, &ui, format!("读取进度失败，原文件已保留：{}", e));
                        set_busy(&shared, &ui, false);
                        continue;
                    }
                };
                if let Err(error)=persist_comment_progress(&output_root,&task,&task.doc().videos) {
                    comment_archive_failure(&shared,&ui,&task,format!("{error:#}"));continue;
                }
                push_log(&shared,&ui,format!("评论自动归档目录：{}",archive::root(&output_root).join(task.uid.to_string()).join("comments").display()));
                active_task(&shared, &ui, &task);
                let pace_min = cfg.interval_ms.clamp(200, 10000) as u64;
                let pace_max = cfg.interval_max_ms.clamp(200, 10000) as u64;
                let mut aborted = false;
                let mut archive_failure = None;
                let mut collection_failure = None;
                for stage_index in 0..task.stages.len() {
                    while !task.stages[stage_index].done {
                        if shared.cancel.load(Ordering::SeqCst) {
                            break;
                        }
                        if shared.cfg.lock().unwrap().sessdata != cfg.sessdata {
                            aborted = true;
                            collection_failure = Some("登录账号已变更，原任务已保留".to_owned());
                            break;
                        }
                        let ty = task.stages[stage_index].ty;
                        let page = task.stages[stage_index].page;
                        let label = checkpoint::label(ty);
                        let progress = format!("正在采集{label}第 {page} 页");
                        set_foot(&ui, progress.clone());
                        set_progress(
                            &ui,
                            stage_index as f32 / task.stages.len().max(1) as f32,
                            progress,
                        );
                        let result = client.up_list_page_retry(
                            ty,
                            page,
                            collection::PAGE_SIZE,
                            0,
                            &shared.cancel,
                            &mut |secs| {
                                push_log(
                                    &shared,
                                    &ui,
                                    format!("{}第 {} 页触发风控，等待 {} 秒", label, page, secs),
                                );
                                emit(&ui,|w|{w.set_verify_title("接口触发风控，正在自动等待".into());w.set_verify_wait_label("风控等待".into());});
                                verify_show(
                                    &ui,
                                    secs,
                                    format!("{}第 {} 页被限速，进度已保留", label, page),
                                );
                            },
                            &mut |left| {
                                verify_tick(&ui, left);
                            },
                        );
                        match result {
                            Ok((page_rows, total)) => {
                                verify_hide(&ui);
                                set_type_total(&shared, &ui, ty, Some(total));
                                let rows_on_page = page_rows.len();
                                let changed:HashSet<_>=page_rows.iter().map(|row|(row.ty.max(1),row.oid)).collect();
                                let mut next=task.clone();next.accept(ty,page,page_rows,total);
                                let partial:Vec<_>=bili::rows_to_videos(&next.rows,cfg.include_replies,self_mid,&self_name).into_iter().filter(|v|changed.contains(&(v.reply_type(),v.aid))).collect();
                                if let Err(error)=persist_comment_progress(&output_root,&next,&partial) {
                                    archive_failure = Some(error);
                                    aborted = true;
                                    break;
                                }
                                task=next;active_task(&shared,&ui,&task);
                                log_foot(&shared, &ui, format!("已保存{label}第 {page} 页 · {rows_on_page} 条评论"));
                                {
                                    let mut doc=shared.doc.lock().unwrap();
                                    for video in partial {doc.merge_video(video);}
                                }
                                refresh_stats(&shared, &ui);
                            }
                            Err(e) => {
                                if !shared.cancel.load(Ordering::SeqCst) {
                                    aborted = true;
                                    collection_failure = Some(format!("{label}第 {page} 页失败，进度已保留：{e}"));
                                }
                                break;
                            }
                        }
                        if !task.stages[stage_index].done {
                            std::thread::sleep(Duration::from_millis(bili::random_delay_ms(
                                pace_min, pace_max,
                            )));
                        }
                    }
                    if aborted || shared.cancel.load(Ordering::SeqCst) {
                        break;
                    }
                }
                verify_hide(&ui);
                if let Some(error) = archive_failure {
                    comment_archive_failure(&shared, &ui, &task, format!("{error:#}"));
                    continue;
                }
                let rows = task.rows.clone();
                if rows.is_empty() {
                    let stopped = shared.cancel.load(Ordering::SeqCst)
                        && !shared.pause_requested.load(Ordering::SeqCst);
                    if task.complete() || stopped {
                        clear_checkpoint(&shared, &ui);
                    }
                    set_busy(&shared, &ui, false);
                    let message = if let Some(failure) = collection_failure {
                        failure
                    } else if task.complete() {
                        "采集完成，暂无评论".to_owned()
                    } else if stopped {
                        "已结束".to_owned()
                    } else {
                        format!("已暂停 · {}", task.summary())
                    };
                    log_foot(&shared, &ui, message);
                    continue;
                }

                // 视频信息统一整合：已删除的稿件不在稿件列表里，
                // 只能用「评论行里带的 title / bvid / cover」+ 稿件列表做差集判断
                set_progress(&ui, 0.97, "整理稿件信息…".into());
                let mut videos =
                    bili::rows_to_videos(&rows, cfg.include_replies, self_mid, &self_name);
                let aids: Vec<u64> = videos
                    .iter()
                    .filter(|v| v.reply_type() == 1)
                    .map(|v| v.aid)
                    .collect();
                let amap = client.archive_map(&aids, &shared.cancel);
                for video in videos
                    .iter_mut()
                    .filter(|v| v.reply_type() == 1 && !shared.cancel.load(Ordering::SeqCst))
                {
                    bili::merge_video_meta(std::slice::from_mut(video), &amap);
                }

                // ---- 动态原文 / IP 属地 / 赞评转 ----
                // 评论管理只给评论，动态本身的正文和计数要单独从动态列表取；
                // basic.comment_type + comment_id_str 正好能和评论行的 oid 对上。
                let dyn_n = videos.iter().filter(|v| v.is_dynamic()).count();
                if dyn_n > 0 && !shared.cancel.load(Ordering::SeqCst) {
                    push_log(
                        &shared,
                        &ui,
                        format!("取 {} 条动态的原文 / IP / 赞评转…", dyn_n),
                    );
                    let feed = client.dynamic_feed(self_mid, &shared.cancel, |page, n| {
                        set_progress(
                            &ui,
                            0.965,
                            format!("动态列表 第 {} 页 · 累计 {} 条", page, n),
                        );
                    });
                    match feed {
                        Ok(list) => {
                            let mut map: HashMap<(u32, u64), bili::DynamicInfo> = HashMap::new();
                            let listed = list.len();
                            for d in list {
                                map.insert((d.comment_type, d.comment_id), d);
                            }
                            let mut hit = 0usize;
                            for v in videos.iter_mut().filter(|v| v.is_dynamic()) {
                                let key = (v.comment_type, v.aid);
                                if let Some(d) = map.get(&key) {
                                    hit += 1;
                                    v.dynamic_id = d.dynamic_id;
                                    v.text = d.text.clone();
                                    if v.title.is_empty() && !d.title.is_empty() {
                                        v.title = d.title.clone();
                                    }
                                    v.text_pics = d.pics.clone();
                                    v.ip_location = d.ip_location.clone();
                                    v.like_count = d.like;
                                    v.forward_count = d.forward;
                                    v.orig_text = d.orig_text.clone();
                                    v.orig_author = d.orig_author.clone();
                                    if d.comment > v.comment_total {
                                        v.comment_total = d.comment;
                                    }
                                    if v.publish_time == 0 && d.pub_ts > 0 {
                                        v.publish_time = d.pub_ts;
                                        v.publish_time_text = ts_text(d.pub_ts);
                                    }
                                    if v.url.is_empty() && !d.url.is_empty() {
                                        v.url = d.url.clone();
                                    }
                                    if v.cover.is_empty() {
                                        v.cover = d.pics.first().cloned().unwrap_or_default();
                                    }
                                    v.status = "public".into();
                                    v.status_text = String::new();
                                } else if listed > 0 {
                                    v.status = "deleted".into();
                                    v.status_text = "已删除".into();
                                }
                            }
                            push_log(
                                &shared,
                                &ui,
                                format!(
                                    "动态信息补齐 {}/{} 条（列表共 {} 条）",
                                    hit, dyn_n, listed
                                ),
                            );
                        }
                        Err(e) => push_log(&shared, &ui, format!("动态列表获取失败：{}", e)),
                    }
                }

                // IP 属地：创作中心接口不带，只能对公开稿件走公开评论接口补
                let public_n = videos
                    .iter()
                    .filter(|v| v.status == "public" && !v.comments.is_empty())
                    .count();
                if public_n > 0 && !shared.cancel.load(Ordering::SeqCst) {
                    push_log(
                        &shared,
                        &ui,
                        format!("补全 IP 属地（{} 个公开稿件；已删除 / 仅自己可见的评论接口已关闭，取不到）", public_n),
                    );
                    let (loc_n, loc_total) =
                        client.fill_locations(&mut videos, &shared.cancel, |i, t, n| {
                            set_progress(
                                &ui,
                                0.98,
                                format!("补 IP 属地 {}/{} · 已补 {} 条", i + 1, t, n),
                            );
                        });
                    if loc_total > 0 {
                        push_log(
                            &shared,
                            &ui,
                            format!(
                                "IP 属地补全：{}/{} 条（其余评论没有公开 IP）",
                                loc_n, loc_total
                            ),
                        );
                    }
                }

                // Save supplemented titles, content and IP data before clearing a completed task.
                if let Err(error)=persist_comment_progress(&output_root,&task,&videos) {
                    comment_archive_failure(&shared,&ui,&task,format!("{error:#}"));continue;
                }
                let exported = videos.clone();
                let hidden = videos.iter().filter(|v| v.is_hidden()).count();
                {
                    let mut doc = shared.doc.lock().unwrap();
                    for v in videos {
                        doc.merge_video(v);
                    }
                }
                refresh_stats(&shared, &ui);
                let stats = shared.doc.lock().unwrap().stats.clone();
                push_log(
                    &shared,
                    &ui,
                    format!(
                        "整合完成：{} 个稿件 / {} 条主评论 / {} 条回复",
                        stats.videos, stats.comments, stats.replies
                    ),
                );
                if hidden > 0 {
                    push_log(
                        &shared,
                        &ui,
                        format!(
                            "其中 {} 个稿件已被删除或仅自己可见，稿件列表里看不到，但评论照样导出",
                            hidden
                        ),
                    );
                }
                let stopped = shared.cancel.load(Ordering::SeqCst)
                    && !shared.pause_requested.load(Ordering::SeqCst);
                if task.complete() || stopped {
                    clear_checkpoint(&shared, &ui);
                } else {
                    active_task(&shared, &ui, &task);
                }
                set_busy(&shared, &ui, false);
                let message = if let Some(failure) = collection_failure {
                    failure
                } else if task.complete() {
                    "采集完成".to_owned()
                } else if stopped {
                    "已结束".to_owned()
                } else {
                    format!("已暂停 · {}", task.summary())
                };
                set_progress(&ui, if task.complete() { 1.0 } else { 0.0 }, message.clone());
                set_status(&ui, message.clone(), task.complete());
                log_foot(
                    &shared,
                    &ui,
                    format!(
                        "{} · {} 个稿件 / {} 条评论 / {} 条回复",
                        message, stats.videos, stats.comments, stats.replies
                    ),
                );
                process_comment_resources(&shared, &ui, &exported,&output_root);
            }

            Cmd::Export { path } => {
                let content = {
                    let doc = shared.doc.lock().unwrap();
                    export::to_json(&doc)
                };
                match content.and_then(|c| export::write_file(&path, &c)) {
                    Ok(_) => {
                        let name = path
                            .file_name()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_default();
                        push_log(&shared, &ui, format!("已导出：{}", path.display()));
                        set_foot(&ui, format!("已导出 {}", name));
                        set_status(&ui, "已导出".into(), true);
                        *shared.last_export.lock().unwrap() = Some(path.clone());

                    }
                    Err(e) => {
                        push_log(&shared, &ui, format!("导出失败：{}", e));
                        set_foot(&ui, "导出失败".into());
                    }
                }
            }
        }
    }
}

fn split_tokens(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || c == ',' || c == '，' || c == ';' || c == '；')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

// ---------------- 命令行模式（无界面，可脚本化） ----------------
fn run_cli() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command: Option<fn(&[String]) -> i32> = match args.first().map(|s| s.as_str()) {
        Some("serve" | "web") => Some(cli_serve),
        Some("login") => Some(cli_login),
        Some("crawl") => None,
        Some("-h") | Some("--help") | Some("help") => {
            print_help();
            return Some(0);
        }
        _ => return None,
    };
    bili::set_proxy(&Cfg::load().proxy);
    if let Some(command) = command {
        return Some(command(&args[1..]));
    }
    let mut targets: Vec<String> = Vec::new();
    let mut out = String::new();
    let mut from_page = 1u64;
    let mut to_page = 0u64;
    let mut replies = false;
    let mut all = false;
    let mut merge = false;
    let mut cookie: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        let next = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--out" | "-o" => {
                out = next;
                i += 2;
            }
            "--format" | "-f" => {
                eprintln!("提示：现在只导出 JSON，--format 已忽略");
                i += 2;
            }
            "--from" | "--start-page" => {
                from_page = next.parse().unwrap_or(1).max(1);
                i += 2;
            }
            "--to" => {
                to_page = next.parse().unwrap_or(0);
                i += 2;
            }
            "--replies" => {
                replies = true;
                i += 1;
            }
            "--all" => {
                all = true;
                i += 1;
            }
            "--merge" => {
                merge = true;
                i += 1;
            }
            "--cookie" => {
                cookie = Some(next);
                i += 2;
            }
            other => {
                if !other.starts_with('-') {
                    targets.push(other.to_string());
                }
                i += 1;
            }
        }
    }
    if out.is_empty() {
        out = default_export_name();
    }
    if all {
        return Some(cli_crawl_all(
            out, replies, cookie, from_page, to_page, merge,
        ));
    }
    if targets.is_empty() {
        eprintln!("用法:");
        eprintln!("  biliarchive crawl BV1xx411c7mD [BV...] [--out 文件] [--replies]");
        eprintln!("  biliarchive crawl --all [--out 文件] [--from 1] [--to 140] [--merge] [--cookie SESSDATA]");
        eprintln!("  biliarchive serve --file out.json [--port 8686] [--open]");
        return Some(2);
    }

    let mut client = match bili::Bili::new(cookie.unwrap_or_else(|| Cfg::load().sessdata)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("初始化失败: {}", e);
            return Some(1);
        }
    };
    apply_saved_fingerprint(&mut client);
    if let Err(e) = client.refresh_wbi() {
        eprintln!("WBI 初始化失败: {}", e);
        return Some(1);
    }
    let cancel = AtomicBool::new(false);
    let mut doc = ArchiveDoc::default();
    for t in &targets {
        let mut video = match client.video_info(t) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("跳过 {}: {}", t, e);
                continue;
            }
        };
        println!("采集: {} ({})", video.title, video.bvid);
        match client.comments_of(&mut video, replies, &cancel, |n, total| {
            println!("  {} / {}", n, total)
        }) {
            Ok(_) => {
                println!(
                    "  完成 {} 条主评论, {} 条回复",
                    video.comments.len(),
                    video
                        .comments
                        .iter()
                        .map(|c| c.replies.len())
                        .sum::<usize>()
                );
                doc.merge_video(video);
            }
            Err(e) => eprintln!("  失败: {}", e),
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    doc.recount();
    let content = match export::to_json(&doc) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("序列化失败: {}", e);
            return Some(1);
        }
    };
    let path = PathBuf::from(&out);
    match export::write_file(&path, &content) {
        Ok(_) => {
            println!(
                "已导出 {} ({} 稿件 / {} 评论 / {} 回复)",
                path.display(),
                doc.stats.videos,
                doc.stats.comments,
                doc.stats.replies
            );
            Some(0)
        }
        Err(e) => {
            eprintln!("写入失败: {}", e);
            Some(1)
        }
    }
}

/// 无界面全量采集：走创作中心评论管理接口，含已删除稿件
fn cli_crawl_all(
    out: String,
    include_replies: bool,
    cookie: Option<String>,
    from_page: u64,
    to_page: u64,
    merge: bool,
) -> i32 {
    let cookie = cookie.unwrap_or_else(|| Cfg::load().sessdata);
    if cookie.trim().is_empty() {
        eprintln!("全量模式需要 SESSDATA：先在界面里填写一次，或用 --cookie 指定");
        return 2;
    }
    let mut client = match bili::Bili::new(cookie) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("初始化失败: {}", e);
            return 1;
        }
    };
    apply_saved_fingerprint(&mut client);
    let (mid, name) = client.me();
    archive::set_account(mid);
    if mid == 0 {
        eprintln!("SESSDATA 无效或已过期");
        return 1;
    }
    println!("账号: {} ({})", name, mid);

    let ps = collection::PAGE_SIZE;
    // CLI 创作中心页码也严格使用 10 条/页。
    let batch_span = 1;
    let official_from = from_page.max(1);
    let official_to = to_page;
    let from_batch = (official_from - 1) / batch_span + 1;
    let to_batch = if official_to > 0 {
        (official_to - 1) / batch_span + 1
    } else {
        0
    };
    let mut rows: Vec<bili::CommentRow> = Vec::new();
    let mut seen: HashSet<u64> = HashSet::new();
    let mut total;
    let mut pn = from_batch;
    let cancel = AtomicBool::new(false);
    if official_to > 0 {
        println!(
            "抓取官方第 {}–{} 页（每批 {} 条）",
            official_from, official_to, ps
        );
    } else {
        println!(
            "从官方第 {} 页抓到最后一页（每批 {} 条）",
            official_from, ps
        );
    }
    loop {
        match client.up_list_page_retry(
            1,
            pn,
            ps,
            0,
            &cancel,
            &mut |secs: u64| {
                println!("  [风控] 第 {} 批限速，等待 {} 秒后重试…", pn, secs);
            },
            &mut |_left: u64| {},
        ) {
            Ok((page_rows, t)) => {
                total = t;
                let got = page_rows.len();
                for r in page_rows {
                    if seen.insert(r.rpid) {
                        rows.push(r);
                    }
                }
                let batches_total = if total > 0 { (total + ps - 1) / ps } else { 0 };
                println!(
                    "  第 {}/{} 批：累计 {}/{} 条",
                    pn,
                    batches_total,
                    rows.len(),
                    total
                );
                if got == 0
                    || (to_batch > 0 && pn >= to_batch)
                    || (total > 0 && rows.len() as u64 >= total)
                    || (batches_total > 0 && pn >= batches_total)
                {
                    break;
                }
                pn += 1;
                std::thread::sleep(Duration::from_millis(800));
            }
            Err(e) => {
                eprintln!("  第 {} 批失败: {}", pn, e);
                break;
            }
        }
    }
    if rows.is_empty() {
        eprintln!("没有拿到评论");
        return 1;
    }

    println!("整理稿件信息…");
    let mut videos = bili::rows_to_videos(&rows, include_replies, mid, &name);
    let aids: Vec<u64> = videos.iter().map(|v| v.aid).collect();
    let amap = client.archive_map(&aids, &cancel);
    bili::merge_video_meta(&mut videos, &amap);
    let public_n = videos
        .iter()
        .filter(|v| v.status == "public" && !v.comments.is_empty())
        .count();
    if public_n > 0 {
        println!("补全 IP 属地（{} 个公开稿件）…", public_n);
        let (loc, all) = client.fill_locations(&mut videos, &cancel, |i, t, _| {
            println!("  公开稿件 {}/{}", i + 1, t)
        });
        println!(
            "IP 属地 {}/{} 条（已删除 / 仅自己可见的评论接口已关闭，取不到）",
            loc, all
        );
    }
    let hidden = videos.iter().filter(|v| v.is_hidden()).count();

    let path = PathBuf::from(&out);
    // --merge：把这次抓到的页合并进已有文件（断点续抓 / 分段抓时用）
    let mut doc = if merge && path.exists() {
        match export::load_doc(&path) {
            Ok(d) => {
                println!(
                    "合并到已有文件：{}（{} 稿件 / {} 评论 / {} 回复）",
                    path.display(),
                    d.stats.videos,
                    d.stats.comments,
                    d.stats.replies
                );
                d
            }
            Err(e) => {
                eprintln!("已有文件读取失败，将覆盖：{}", e);
                ArchiveDoc::default()
            }
        }
    } else {
        ArchiveDoc::default()
    };
    for v in videos {
        doc.merge_video(v);
    }
    doc.recount();
    if hidden > 0 {
        println!("其中 {} 个稿件已删除或仅自己可见（稿件列表里没有）", hidden);
    }
    let content = match export::to_json(&doc) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("序列化失败: {}", e);
            return 1;
        }
    };
    match export::write_file(&path, &content) {
        Ok(_) => {
            println!(
                "已导出 {} ({} 稿件 / {} 评论 / {} 回复)",
                path.display(),
                doc.stats.videos,
                doc.stats.comments,
                doc.stats.replies
            );
            0
        }
        Err(e) => {
            eprintln!("写入失败: {}", e);
            1
        }
    }
}

/// 命令行扫码登录：终端里直接画二维码，扫完把 Cookie 写进配置
fn cli_login(args: &[String]) -> i32 {
    let save = !args.iter().any(|a| a == "--no-save");
    let login = match qrlogin::QrLogin::new() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("初始化失败：{}", e);
            return 1;
        }
    };
    println!("用 B 站手机客户端扫码（我的 → 右上角「扫一扫」）\n");

    for _round in 0..20 {
        let (url, key) = match login.generate() {
            Ok(v) => v,
            Err(e) => {
                eprintln!("获取二维码失败：{}", e);
                return 1;
            }
        };
        if let Some(art) = qrlogin::qr_ascii(&url) {
            println!("{}", art);
        } else {
            println!("二维码内容：{}\n", url);
        }
        println!("等待扫码…");

        let mut scanned = false;
        for _ in 0..90 {
            std::thread::sleep(qrlogin::POLL_INTERVAL);
            match login.poll(&key) {
                Ok(qrlogin::Poll::Waiting) => {}
                Ok(qrlogin::Poll::Scanned) => {
                    if !scanned {
                        scanned = true;
                        println!("已扫码，请在手机上点「确认登录」…");
                    }
                }
                Ok(qrlogin::Poll::Expired) => {
                    println!("二维码已过期，正在换一张…\n");
                    break;
                }
                Ok(qrlogin::Poll::Done { cookie, mid }) => {
                    println!("登录成功，UID {}", mid);
                    // Credentials are deliberately not printed to stdout.
                    if save {
                        let mut cfg = Cfg::load();
                        cfg.sessdata = cookie;
                        if cfg.buvid3.is_empty() {
                            cfg.buvid3 = login.buvid3.clone();
                            cfg.buvid4 = login.buvid4.clone();
                        }
                        if let Err(e) = cfg.save() {
                            eprintln!("登录信息未保存：{}", e);
                            return 1;
                        }
                        match Cfg::file() {
                            Some(p) => println!("\n已写入配置：{}", p.display()),
                            None => println!("\n（没找到配置目录，未保存）"),
                        }
                    }
                    return 0;
                }
                Err(e) => {
                    eprintln!("查询扫码状态失败：{}", e);
                    std::thread::sleep(Duration::from_secs(3));
                }
            }
        }
    }
    eprintln!("多次尝试仍未登录，请重试");
    1
}

fn print_help() {
    println!("BiliArchive — 归档哔哩哔哩稿件评论、私信和互动通知");
    println!();
    println!("用法：");
    println!("  biliarchive                                    打开图形界面");
    println!(
        "  biliarchive login [--no-save]                  扫码登录并写入配置（终端画二维码）"
    );
    println!("  biliarchive crawl BV1xx411c7mD [BV...] [--out 文件] [--replies]");
    println!("  biliarchive crawl --all [--out 文件] [--from 1] [--to 140] [--merge] [--cookie SESSDATA]");
    println!("  biliarchive serve --file out.json [--port 8686] [--open]");
    println!();
    println!("serve 会在本机起一个网页，把 JSON 列成可浏览的界面：");
    println!("  /              网页列表");
    println!("  /api/comments/data.json 完整数据（JSON）");
    println!("  /api/comments/posts    稿件摘要列表");
    println!("  /api/comments/posts/123  单个稿件");
    println!("  /api/comments/search?q=关键词  跨稿件搜索评论（含楼中楼）");
}

/// 起本地网页服务，把导出的 JSON 列出来
fn cli_serve(args: &[String]) -> i32 {
    let mut file = String::new();
    let mut port: u16 = 8686;
    let mut open = false;
    let mut i = 0;
    while i < args.len() {
        let next = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--file" | "-f" => {
                file = next;
                i += 2;
            }
            "--port" | "-p" => {
                port = next.parse().unwrap_or(8686);
                i += 2;
            }
            "--open" => {
                open = true;
                i += 1;
            }
            other if !other.starts_with('-') && file.is_empty() => {
                file = other.to_string();
                i += 1;
            }
            _ => i += 1,
        }
    }
    if file.is_empty() {
        eprintln!("用法: biliarchive serve --file out.json [--port 8686] [--open]");
        return 2;
    }
    let path = PathBuf::from(&file);
    if !path.exists() {
        eprintln!("文件不存在: {}", path.display());
        return 2;
    }
    let doc = match export::load_doc(&path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("读取失败: {}", e);
            return 1;
        }
    };
    let (videos, hidden) = (doc.stats.videos, doc.stats.deleted_videos);
    println!(
        "已加载 {}：{} 个稿件（其中 {} 个已隐藏）/ {} 条评论 / {} 条回复",
        path.display(),
        videos,
        hidden,
        doc.stats.comments,
        doc.stats.replies
    );
    let shared = Arc::new(Mutex::new(doc));
    match web::spawn(shared, port) {
        Ok(p) => {
            let base = format!("http://127.0.0.1:{p}");
            let url = preview_url(&base, 0);
            println!("网页已启动: {}", url);
            println!("  数据接口: {base}/api/comments/data.json");
            println!("按 Ctrl+C 结束");
            if open {
                let _ = webbrowser::open(&url);
            }
            loop {
                std::thread::park();
            }
        }
        Err(e) => {
            eprintln!("启动失败: {}", e);
            1
        }
    }
}

// ---------------- 入口 ----------------
/// 启动时若带了导出文件（--file x.json / 直接给路径），先载入界面
fn preload_path() -> Option<PathBuf> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--file" || args[i] == "-f" {
            if let Some(p) = args.get(i + 1) {
                return Some(PathBuf::from(p));
            }
        }
        if !args[i].starts_with('-') {
            let lower = args[i].to_lowercase();
            if lower.ends_with(".json") {
                return Some(PathBuf::from(&args[i]));
            }
        }
        i += 1;
    }
    None
}

fn main() -> Result<(), slint::PlatformError> {
    // Dispatch the worker before opening Slint or the platform credential store.
    if std::env::args().nth(1).as_deref() == Some("--preview-worker") {
        std::process::exit(preview_process::run());
    }
    if let Some(code) = run_cli() {
        std::process::exit(code);
    }

    let Some(_desktop_lock) = single_instance::acquire_or_notify() else {
        return Ok(());
    };

    let cfg = Cfg::load();
    // 先应用代理策略，再创建桌面和启动网络任务。
    bili::set_proxy(&cfg.proxy);
    let ui = AppWindow::new()?;

    ui.set_sessdata(cfg.sessdata.clone().into());
    ui.set_include_replies(cfg.include_replies);
    ui.set_foot_text("就绪 · 扫码登录，或直接粘贴 BV 号".into());
    ui.set_web_port_text(cfg.web_port.to_string().into());
    ui.set_archive_precache(cfg.archive_precache);
    ui.set_archive_workers_auto(local_read::workers() as i32);
    ui.set_export_dir_text(export_dir_of(&cfg).into());
    ui.set_resource_avatars(cfg.resource_policy.avatars);ui.set_resource_covers(cfg.resource_policy.covers);ui.set_resource_content_images(cfg.resource_policy.content_images);
    ui.set_page_delay_text(cfg.interval_ms.to_string().into());
    ui.set_page_delay_max_text(cfg.interval_max_ms.to_string().into());

    let shared = Arc::new(Shared::new(cfg));
    range_ui::render(&ui, &shared.range_session.lock().unwrap());
    let (tx, rx) = channel::<Cmd>();
    let weak = ui.as_weak();
    let config_error_timer = slint::Timer::default();
    {
        let weak = weak.clone();
        let shared = shared.clone();
        config_error_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(500),
            move || {
                let errors = std::mem::take(&mut *CONFIG_ERRORS.lock().unwrap());
                for error in errors {
                    push_log(&shared, &weak, &error);
                    set_foot(&weak, error);
                }
            },
        );
    }

    // 命令行里给了导出文件就直接载入，省掉手动导入
    if let Some(path) = preload_path() {
        match export::load_doc(&path) {
            Ok(doc) => {
                let (v, c, r) = (doc.stats.videos, doc.stats.comments, doc.stats.replies);
                *shared.doc.lock().unwrap() = doc;
                refresh_stats(&shared, &weak);
                push_log(&shared, &weak, format!("已载入 {}", path.display()));
                set_foot(&weak, format!("{} 个稿件 / {} 条评论 / {} 条回复", v, c, r));
                set_status(&weak, "数据已载入".into(), true);
            }
            Err(e) => push_log(&shared, &weak, format!("载入失败：{}", e)),
        }
    }

    {
        let shared = shared.clone();
        let weak = weak.clone();
        std::thread::spawn(move || worker(rx, shared, weak));
    }

    offline::install(&ui,&shared,&tx,&weak);
    // Saved credentials use the same validation/listing pipeline as a new login.
    if offline::has_saved_selection(){let _=tx.send(Cmd::Offline(offline::Action::Restore));}else{enqueue_archive_read(&shared,&weak,&tx);}
    // Apply synchronously before the first window frame, rather than waiting for a worker event.
    range_ui::render(&ui,&shared.range_session.lock().unwrap());

    // ---- 读取稿件 ----
    {
        let tx = tx.clone();
        let shared = shared.clone();
        let weak = weak.clone();
        ui.on_read_archives(move |sessdata: SharedString| {
            if shared.offline.load(Ordering::SeqCst) {
                log_foot(&shared, &weak, "离线账号不能读取线上稿件");
                return;
            }
            let value = sessdata.to_string();
            shared.qr_gen.fetch_add(1, Ordering::SeqCst);
            emit(&weak, |w| w.set_qr_visible(false));
            {
                let mut cfg = shared.cfg.lock().unwrap();
                if value.trim().is_empty() {
                    cfg.buvid3.clear();
                    cfg.buvid4.clear();
                    cfg.secrets_blocked = false;
                    cfg.clear_login = true;
                }
                cfg.sessdata = value.clone();
                cfg.save_reported();
            }
            if value.trim().is_empty() {
                set_account(&shared,&weak,0);
                push_log(&shared, &weak, "请先扫码登录，再试");
                return;
            }
            push_log(&shared, &weak, "正在读取创作中心稿件列表…");
            enqueue_archive_read(&shared,&weak,&tx);
            if let Some(w)=weak.upgrade(){range_ui::render(&w,&shared.range_session.lock().unwrap());}
        });
    }

    // ---- 扫码登录 ----
    {
        let shared = shared.clone();
        let weak = weak.clone();
        let tx_qr = tx.clone();
        ui.on_qr_login(move || {
            if shared.busy.load(Ordering::SeqCst) {
                return;
            }
            offline::leave(&shared,&weak);
            let my_gen = shared.qr_gen.fetch_add(1, Ordering::SeqCst) + 1;
            emit(&weak, |w| {
                w.set_qr_visible(true);
                w.set_qr_status("正在获取二维码…".into());
                w.set_qr_empty(true);
            });
            let shared = shared.clone();
            let weak = weak.clone();
            let tx = tx_qr.clone();
            std::thread::spawn(move || run_qr_login(shared, weak, tx, my_gen));
        });
    }
    {
        let shared = shared.clone();
        let weak = weak.clone();
        ui.on_qr_cancel(move || {
            // 代次 +1 让还在轮询的线程自行退出
            shared.qr_gen.fetch_add(1, Ordering::SeqCst);
            emit(&weak, |w| w.set_qr_visible(false));
        });
    }

    // ---- 添加视频 ----
    {
        let tx = tx.clone();
        let shared = shared.clone();
        let weak = weak.clone();
        ui.on_add_video(move |text: SharedString| {
            let tokens = split_tokens(&text.to_string());
            if tokens.is_empty() {
                push_log(&shared, &weak, "请输入 BV 号或视频链接");
                return;
            }
            emit(&weak, |w| w.set_new_video_text("".into()));
            let _ = tx.send(Cmd::AddVideos(tokens));
        });
    }

    // ---- 勾选 ----
    {
        let shared = shared.clone();
        let weak = weak.clone();
        ui.on_toggle_video(move |idx: i32| {
            {
                let mut rows = shared.rows.lock().unwrap();
                if let Some(row) = rows.get_mut(idx as usize) {
                    row.1 = !row.1;
                }
            }
            refresh_rows(&shared, &weak);
        });
    }

    {
        let shared = shared.clone();
        let weak = weak.clone();
        ui.on_toggle_all(move || {
            {
                let mut rows = shared.rows.lock().unwrap();
                let all_checked = !rows.is_empty() && rows.iter().all(|(_, c)| *c);
                for row in rows.iter_mut() {
                    row.1 = !all_checked;
                }
            }
            refresh_rows(&shared, &weak);
        });
    }

    // ---- 开始 / 停止 ----
    {
        let tx = tx.clone();
        let shared = shared.clone();
        let weak = weak.clone();
        ui.on_start(move || {
            if shared.offline.load(Ordering::SeqCst) {
                log_foot(&shared, &weak, "请先在线验证原账号后采集");
                return;
            }
            if shared.busy.load(Ordering::SeqCst) {
                return;
            }
            let Some(w) = weak.upgrade() else {
                return;
            };
            let scope = w.get_scope();
            let mut cfg = shared.cfg.lock().unwrap();
            if scope == 0 {
                let mut ranges = match ranges_from_ui(&w) {
                    Ok(r) => r,
                    Err(e) => {
                        drop(cfg);
                        port_alert(&weak, e.to_string());
                        return;
                    }
                };
                for (&ty,state) in &shared.range_session.lock().unwrap().ranges {
                    if !state.available() {
                        let mut saved=state.disabled_preference().or_else(||cfg.ranges.get(&ty).cloned()).unwrap_or_default();saved.enabled=false;ranges.insert(ty,saved);
                    }
                }
                if let Err(e) = collection::validate(&ranges, true) {
                    drop(cfg);
                    port_alert(&weak, e.to_string());
                    return;
                }
                cfg.include_dynamics = ranges.iter().any(|(&ty, r)| ty != 1 && r.enabled);
                cfg.ranges = ranges;
            }
            cfg.include_replies = w.get_include_replies();
            cfg.save_reported();
            drop(cfg);
            let _ = tx.send(if scope == 0 { Cmd::CrawlUp } else { Cmd::Start });
        });
    }

    {
        let shared = shared.clone();
        let weak = weak.clone();
        ui.on_pause(move || {
            pause_collection(&shared,&weak);
        });
    }

    {
        let shared = shared.clone();
        let weak = weak.clone();
        ui.on_stop(move || {
            stop_collection(&shared,&weak);
        });
    }

    {
        let shared = shared.clone();
        let weak = weak.clone();
        ui.on_clear_resume(move || {
            if shared.busy.load(Ordering::SeqCst) {
                return;
            }
            clear_checkpoint(&shared, &weak);
            log_foot(&shared, &weak, "已清除断点，下次将从头开始采集");
        });
    }

    // ---- 导出 ----
    {
        let tx = tx.clone();
        let shared = shared.clone();
        let weak = weak.clone();
        ui.on_export_clicked(move || {
            let dir=export_dir_of(&shared.cfg.lock().unwrap());
            let mut dialog = rfd::FileDialog::new()
                .set_title("导出评论数据（JSON）")
                .set_file_name(default_export_name());
            if !dir.is_empty() {
                dialog = dialog.set_directory(&dir);
            }
            let picked = dialog.add_filter("JSON 文档", &["json"]).save_file();
            if let Some(mut path) = picked {
                if path.extension().is_none() {
                    path.set_extension("json");
                }
                let _ = tx.send(Cmd::Export { path });
            }
            let _ = &weak;
        });
    }

    // ---- 网页预览（入口：设置里的「打开网页」） ----
    // 已经起过的服务地址，重复点击只是再打开一次浏览器，不重复占用端口
    let web_url: Arc<Mutex<Option<WebHandle>>> = Arc::new(Mutex::new(None));
    {
        let shared = shared.clone();
        let weak = weak.clone();
        let web_url = web_url.clone();
        ui.on_settings_open_web(move || {
            let port = shared.cfg.lock().unwrap().web_port;
            ensure_web(&shared, &weak, &web_url, port, true);
        });
    }

    {
        let shared=shared.clone(); let weak=weak.clone(); let web_url=web_url.clone();
        ui.on_preview_status_clicked(move||{
            let port=shared.cfg.lock().unwrap().web_port;
            ensure_web(&shared,&weak,&web_url,port,true);
        });
    }

    // ---- 设置里的 启动 / 停止 ----
    {
        let shared = shared.clone();
        let weak = weak.clone();
        let web_url = web_url.clone();
        ui.on_settings_toggle_web(move || {
            if web_url.lock().unwrap().is_some() {
                stop_web(&shared, &weak, &web_url);
                return;
            }
            let port = match weak
                .upgrade()
                .and_then(|w| w.get_web_port_text().trim().parse::<u16>().ok())
            {
                Some(p) if p >= 1024 => p,
                _ => {
                    port_alert(&weak, "端口必须为 1024–65535".into());
                    return;
                }
            };
            ensure_web(&shared, &weak, &web_url, port, false);
        });
    }



    // ---- 导入 ----


    // ---- 打开目录 ----
    {
        let shared = shared.clone();
        ui.on_open_dir(move || {
            let dir = {
                let cfg = shared.cfg.lock().unwrap();
                export_dir_of(&cfg)
            };
            if dir.is_empty() {
                return;
            }
            open_path(&dir);
        });
    }


    {let tx=tx.clone();let shared=shared.clone();ui.on_process_resources(move ||{if !shared.busy.load(Ordering::SeqCst){let _=tx.send(Cmd::ProcessResources);}});}

    // ---- 设置 ----
    {
        let shared = shared.clone();
        let weak = weak.clone();
        ui.on_settings_open(move || {
            let (port, dir, delay, delay_max, precache, workers) = {
                let cfg = shared.cfg.lock().unwrap();
                (
                    cfg.web_port.to_string(),
                    export_dir_of(&cfg),
                    cfg.interval_ms.to_string(),
                    cfg.interval_max_ms.to_string(),
                    cfg.archive_precache,
                    cfg.archive_worker_threads.map(|count| count.to_string()).unwrap_or_default(),
                )
            };
            let policy=shared.cfg.lock().unwrap().resource_policy.clone();
            emit(&weak, move |w| {
                w.set_web_port_text(port.into());
                w.set_export_dir_text(dir.into());
                w.set_resource_avatars(policy.avatars);w.set_resource_covers(policy.covers);w.set_resource_content_images(policy.content_images);
                w.set_page_delay_text(delay.into());
                w.set_page_delay_max_text(delay_max.into());
                w.set_archive_precache(precache);
                w.set_archive_workers_text(workers.into());
                w.set_settings_visible(true);
            });
        });
    }

    {
        let shared = shared.clone();
        let weak = weak.clone();
        let settings_tx = tx.clone();
        ui.on_settings_apply(move || {
            let (port_txt, dir, delay_txt, delay_max_txt, workers_txt) = match weak.upgrade() {
                Some(w) => (
                    w.get_web_port_text().to_string(),
                    w.get_export_dir_text().to_string(),
                    w.get_page_delay_text().to_string(),
                    w.get_page_delay_max_text().to_string(),
                    w.get_archive_workers_text().to_string(),
                ),
                None => return,
            };
            let port = match port_txt.trim().parse::<u16>() {
                Ok(p) if p >= 1024 => p,
                _ => {
                    port_alert(&weak, "端口必须为 1024–65535".into());
                    return;
                }
            };
            let parse_delay = |text: &str| {
                text.trim()
                    .parse::<i64>()
                    .ok()
                    .filter(|n| (200..=10000).contains(n))
            };
            let (low, high) = match (parse_delay(&delay_txt), parse_delay(&delay_max_txt)) {
                (Some(low), Some(high)) if high >= low => (low, high),
                _ => {
                    port_alert(
                        &weak,
                        "翻页间隔必须为 200–10000 毫秒，且上限不能小于下限".into(),
                    );
                    return;
                }
            };
            let workers = match local_read::parse_workers(&workers_txt) {
                Ok(workers) => workers,
                Err(message) => {
                    port_alert(&weak, message.into());
                    return;
                }
            };
            let mut cfg = shared.cfg.lock().unwrap();
            let mut next = cfg.clone();
            next.archive_worker_threads = workers;
            if let Some(w) = weak.upgrade() {
                let precache = w.get_archive_precache();
                if cfg.archive_precache != precache
                    && (shared.busy.load(Ordering::SeqCst) || shared.active_task.load(Ordering::SeqCst) != 0)
                {
                    port_alert(&weak, "请等待当前任务结束后再切换归档读取模式".into());
                    return;
                }
                next.archive_precache = precache;
            }
            next.web_port = port;
            next.interval_ms = low;
            next.interval_max_ms = high;
            if !shared.offline.load(Ordering::SeqCst){next.export_dir = dir.clone();}
            if let Some(w)=weak.upgrade(){next.resource_policy=archive::ResourcePolicy{avatars:w.get_resource_avatars(),covers:w.get_resource_covers(),content_images:w.get_resource_content_images()};}
            match next.save() {
                Ok(()) => {
                    let changed_root=!shared.offline.load(Ordering::SeqCst)&&dir!=export_dir_of(&cfg);
                    let changed_mode = cfg.archive_precache != next.archive_precache;
                    *cfg = next;
                    drop(cfg);
                    if changed_mode { let _ = settings_tx.send(Cmd::ArchiveModeChanged); }
                    if changed_root {if let Err(e)=offline::forget_active(){push_log(&shared,&weak,e.to_string());}}
                    log_foot(&shared, &weak, "设置已保存");
                }
                Err(e) => {
                    let message = format!("保存设置失败：{}", e);
                    port_alert(&weak, message.clone());
                    log_foot(&shared, &weak, message);
                }
            }
        });
    }

    {
        let shared = shared.clone();
        let weak = weak.clone();
        ui.on_settings_close(move || {
            // 关闭即放弃未保存的改动：界面字段恢复成已保存的配置值。
            // 只有点「保存」触发 settings-apply 才落盘。
            if let Some(w) = weak.upgrade() {
                let cfg = shared.cfg.lock().unwrap();
                let port = cfg.web_port.to_string();
                let dir = export_dir_of(&cfg);
                let policy = cfg.resource_policy.clone();
                let delay = cfg.interval_ms.to_string();
                let delay_max = cfg.interval_max_ms.to_string();
                drop(cfg);
                w.set_resource_avatars(policy.avatars);w.set_resource_covers(policy.covers);w.set_resource_content_images(policy.content_images);
                w.set_settings_visible(false);
                w.set_web_port_text(port.into());
                w.set_export_dir_text(dir.into());
                w.set_page_delay_text(delay.into());
                w.set_page_delay_max_text(delay_max.into());
            }
        });
    }

    {
        let shared = shared.clone();
        let weak = weak.clone();
        ui.on_settings_pick_dir(move || {
            let start = {
                let cfg = shared.cfg.lock().unwrap();
                export_dir_of(&cfg)
            };
            let mut dialog = rfd::FileDialog::new().set_title("选择导出目录");
            if !start.is_empty() {
                dialog = dialog.set_directory(&start);
            }
            if let Some(dir) = dialog.pick_folder() {
                let dir = dir.to_string_lossy().to_string();
                emit(&weak, move |w| w.set_export_dir_text(dir.into()));
            }
        });
    }

    {
        let shared = shared.clone();
        let weak = weak.clone();
        ui.on_settings_open_dir(move || {
            let dir = {
                let cfg = shared.cfg.lock().unwrap();
                export_dir_of(&cfg)
            };
            if dir.is_empty() {
                push_log(&shared, &weak, "还没设置导出目录");
                return;
            }
            if std::fs::create_dir_all(&dir).is_err() {
                push_log(&shared, &weak, format!("打不开：{}", dir));
                return;
            }
            open_path(&dir);
        });
    }

    {
        let shared=shared.clone();let weak=weak.clone();
        ui.on_range_edited(move |ty| {if let Some(w)=weak.upgrade(){
            if let Some((from,to,_))=range_input(&w,ty as u32) {
                let mut session=shared.range_session.lock().unwrap();
                if let Some(state)=session.ranges.get_mut(&(ty as u32)){state.edit(&from,&to);}
                range_ui::render(&w,&session);
            }
        }});
    }
    {
        let shared=shared.clone();let weak=weak.clone();
        ui.on_range_all(move |ty| {if shared.busy.load(Ordering::SeqCst){return;}if let Some(w)=weak.upgrade(){
            let mut session=shared.range_session.lock().unwrap();
            if let Some(state)=session.ranges.get_mut(&(ty as u32)){state.all();}
            range_ui::render(&w,&session);
        }});
    }
    {
        let shared=shared.clone();let weak=weak.clone();
        ui.on_range_enabled(move |ty,enabled| {if let Some(w)=weak.upgrade(){
            let mut session=shared.range_session.lock().unwrap();
            if let Some(state)=session.ranges.get_mut(&(ty as u32)){state.choose(enabled);}
            range_ui::render(&w,&session);
        }});
    }
    {
        let shared=shared.clone();let weak=weak.clone();let tx=tx.clone();
        ui.on_range_retry(move |ty| {
            if shared.busy.load(Ordering::SeqCst)||shared.loading_local.load(Ordering::SeqCst){return;}
            if shared.account_uid.load(Ordering::SeqCst) == 0 {
                log_foot(&shared, &weak, "请先登录以读取类型总数");
                return;
            }
            let Some(w)=weak.upgrade() else{return;};
            {
                let session=shared.range_session.lock().unwrap();
                let Some(state)=session.ranges.get(&(ty as u32)) else{return;};
                if state.pending{return;}
            }
            let uid=shared.account_uid.load(Ordering::SeqCst);
            let cookie=shared.cfg.lock().unwrap().sessdata.clone();
            let batch=begin_range_read(&shared,&weak,&[ty as u32]);
            range_ui::render(&w,&shared.range_session.lock().unwrap());
            if tx.send(Cmd::QueryTotals {types:vec![ty as u32],uid,cookie,batch}).is_err(){
                shared.range_session.lock().unwrap().finish(batch);render_range_session(&shared,&weak);
            }
        });
    }

    // ---- 风控弹窗：后台继续等 ----
    {
        let weak = weak.clone();
        ui.on_verify_dismiss(move || {
            emit(&weak,|w|{w.set_verify_background(true);w.set_verify_visible(false);});
        });
    }

    messages_ui::install(&ui, &shared, &tx, &weak, &web_url);

    // ---- 默认启动网页服务：没数据也先起，之后采集到的数据会直接出现在页面上 ----
    {
        let port = shared.cfg.lock().unwrap().web_port;
        ensure_web(&shared, &weak, &web_url, port, false);
    }

    let result = ui.run();
    web_url.lock().unwrap().take();
    result
}

// 从 UI 读回选项（避免在回调里直接 borrow 窗口）
fn ui_include(weak: &Weak<AppWindow>) -> bool {
    weak.upgrade()
        .map(|w| w.get_include_replies())
        .unwrap_or(true)
}

#[cfg(test)]
mod config_security_tests {
    use super::*;

    #[test]
    fn settings_save_preserves_unreadable_secrets() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.json");
        let original = serde_json::json!({"sessdata":"enc:broken", "buvid3":"enc:also-broken"});
        secure::write_config(&path, &original).unwrap();
        let mut cfg = Cfg::default();
        cfg.secrets_blocked = true;
        cfg.web_port = 9000;
        // Fingerprint refreshes cannot silently replace an unreadable login.
        cfg.buvid3 = "new-device".into();
        cfg.save_to(&path).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(saved["sessdata"], original["sessdata"]);
        assert_eq!(saved["buvid3"], original["buvid3"]);
        assert_eq!(saved["web_port"], 9000);
        assert!(cfg.secrets_blocked);
    }

    #[test]
    fn invalid_config_is_not_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.json");
        std::fs::write(&path, b"invalid json").unwrap();
        assert!(Cfg::default().save_to(&path).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"invalid json");
    }

    #[cfg(windows)]
    #[test]
    fn save_encrypted_settings_and_explicitly_clear() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.json");

        let mut cfg = Cfg::default();
        cfg.sessdata = "fake-cookie".into();
        cfg.save_to(&path).unwrap();
        let first: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(first["sessdata"].as_str().unwrap().starts_with("enc:"));
        cfg.web_port = 9001;
        cfg.save_to(&path).unwrap();
        let next: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(first["sessdata"], next["sessdata"]);
        cfg.sessdata.clear();
        cfg.clear_login = true;
        cfg.save_to(&path).unwrap();
        let cleared: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(secure::load(&cleared).unwrap(), secure::Secrets::default());
    }
}

fn active_task(shared: &Arc<Shared>, ui: &Weak<AppWindow>, task: &checkpoint::Task) {
    *shared.checkpoint.lock().unwrap() = Some(task.clone());
    refresh_resume(shared, ui);
}

fn clear_checkpoint(shared: &Arc<Shared>, ui: &Weak<AppWindow>) {
    if let Some(task) = shared.checkpoint.lock().unwrap().take() {
        if let Err(e) = task.remove() {
            report_config_error(&format!("清除采集进度失败：{}", e));
        }
    }
    refresh_resume(shared, ui);
}

fn restore_checkpoint(shared: &Arc<Shared>, ui: &Weak<AppWindow>, uid: u64) {
    let cfg = shared.cfg.lock().unwrap().clone();
    let discovery = match checkpoint::latest(uid, &cfg.ranges, cfg.include_replies) {
        Ok(result) => result,
        Err(e) => {
            push_log(shared, ui, format!("读取采集进度失败，原文件已保留：{}", e));
            return;
        }
    };
    for notice in discovery.notices {
        push_log(shared, ui, notice);
    }
    match discovery.task {
        Some(task) => {
            let mut doc = shared.doc.lock().unwrap();
            let has_bodies = doc.stats.comments == 0 || doc.videos.iter().any(|v| !v.comments.is_empty());
            if cfg.archive_precache && has_bodies {
                doc.merge_videos(task.doc().videos);
            }
            drop(doc);
            if task.mode == "selected" {
                let mut rows = shared.rows.lock().unwrap();
                for target in &task.targets {
                    if let Some((v, selected)) = rows
                        .iter_mut()
                        .find(|(v, _)| checkpoint::key(v) == checkpoint::key(target))
                    {
                        *v = target.without_comments();
                        *selected = true;
                    } else {
                        rows.push((target.clone(), true));
                    }
                }
            }
            let scope = if task.mode == "all" { 0 } else { 1 };
            emit(ui, move |w| w.set_scope(scope));
            log_foot(shared, ui, format!("已恢复采集进度：{}", task.summary()));
            active_task(shared, ui, &task);
            refresh_rows(shared, ui);
            refresh_stats(shared, ui);
        }
        None => (),
    }
}

fn set_account(shared: &Arc<Shared>, ui: &Weak<AppWindow>, uid: u64) {
    set_account_with_loading(shared,ui,uid,false);
}
fn set_account_with_loading(shared: &Arc<Shared>, ui: &Weak<AppWindow>, uid: u64, preserve_loading:bool) {
    let old = shared.account_uid.swap(uid, Ordering::SeqCst);
    archive::set_account(uid);
    if old != uid {
        account_ui::reset(shared, ui, uid);
        emit(ui,|w|{w.set_message_groups(false);w.set_message_groups_confirm(false);message_dates::reset(w);});
        *shared.messages.lock().unwrap() = messages::Store::default();
        messages_ui::refresh(shared, ui);
        if !preserve_loading && uid != 0 && shared.cfg.lock().unwrap().archive_precache {
            let _local = LocalReadGuard::messages(shared, ui);
            messages_ui::load_overview(shared, ui, uid);
        }
        let saved_ranges=shared.cfg.lock().unwrap().ranges.clone();
        shared.range_session.lock().unwrap().reset(&saved_ranges,preserve_loading);
        render_range_session(shared,ui);
        shared.rows.lock().unwrap().clear();
        *shared.doc.lock().unwrap() = ArchiveDoc::default();
        *shared.checkpoint.lock().unwrap() = None;
        refresh_rows(shared, ui);
        refresh_stats(shared, ui);
        refresh_resume(shared, ui);
    }
}

fn port_alert(ui: &Weak<AppWindow>, message: String) {
    emit(ui, move |w| {
        w.set_alert_text(message.into());
        w.set_alert_visible(true);
    });
}

#[cfg(test)]
mod settings_component_tests {
    use super::*;
    use slint::platform::{
        software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
        Platform, PointerEventButton, WindowAdapter, WindowEvent,
    };
    use std::{cell::Cell, rc::Rc};

    struct Headless(Rc<MinimalSoftwareWindow>);
    impl Platform for Headless {
        fn create_window_adapter(
            &self,
        ) -> std::result::Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
            Ok(self.0.clone())
        }
    }
    fn click(ui: &AppWindow, x: f32, y: f32) {
        let position = slint::LogicalPosition::new(x, y);
        ui.window().dispatch_event(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
        ui.window().dispatch_event(WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        });
    }
    #[test]
    fn calendar_selection_is_only_written_on_confirm() {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window))).unwrap();
        let ui = AppWindow::new().unwrap();
        message_dates::install(&ui, &ui.as_weak());
        ui.set_message_from("2023-12-31".into());
        ui.invoke_message_calendar_open(0);
        ui.invoke_message_calendar_shift(1);
        assert_eq!(ui.get_message_calendar_year(), 2024);
        assert_eq!(ui.get_message_calendar_month(), 1);
        ui.invoke_message_calendar_month_changed(2024, 2);
        ui.invoke_message_calendar_day_selected(29);
        assert_eq!(ui.get_message_from().as_str(), "2023-12-31");
        assert!(ui.invoke_message_calendar_accept(0, ui.get_message_calendar_date()));
        assert_eq!(ui.get_message_from().as_str(), "2024-02-29");
        assert_eq!(ui.invoke_message_date_display(ui.get_message_from()).as_str(), "2024/02/29");
    }
    #[test]
    fn queued_archive_read_is_loading_before_worker_and_cleans_up_on_exit() {
        let window=MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window))).unwrap();
        let ui=AppWindow::new().unwrap();let weak=ui.as_weak();
        let mut cfg=Cfg::default();cfg.sessdata="test-session".into();
        let shared=Arc::new(Shared::new(cfg));let(tx,rx)=channel();
        assert!(enqueue_archive_read(&shared,&weak,&tx));
        range_ui::render(&ui,&shared.range_session.lock().unwrap());
        assert_eq!(ui.get_range_video_total().as_str(),"读取中…");
        assert!(ui.get_range_video_loading()&&ui.get_range_audio_loading());
        let batch=match rx.try_recv().unwrap(){Cmd::ReadArchives{batch,..}=>batch,_=>panic!("unexpected command")};
        {
            let _guard=RangeReadGuard::new(&shared,&weak,batch);
            complete_range_read(&shared,&weak,batch,1,Some(65));
            assert!(shared.range_session.lock().unwrap().ranges[&11].pending);
        }
        assert!(shared.range_session.lock().unwrap().ranges.values().all(|r|!r.pending));
        assert_eq!(shared.range_session.lock().unwrap().ranges[&1].total,Some(65));
        drop(rx);assert!(!enqueue_archive_read(&shared,&weak,&tx));
        assert!(shared.range_session.lock().unwrap().ranges.values().all(|r|!r.pending));
        shared.cfg.lock().unwrap().sessdata.clear();
        assert!(!enqueue_archive_read(&shared,&weak,&tx));
        assert!(shared.range_session.lock().unwrap().ranges.values().all(|r|!r.pending));
    }
    #[test]
    fn collection_links_only_appear_in_selected_mode_and_footer_is_clickable() {
        let window=MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
        let ui=AppWindow::new().unwrap();window.set_size(slint::PhysicalSize::new(1040,700));
        let reads=Rc::new(Cell::new(0));let counter=reads.clone();ui.on_read_archives(move |_|counter.set(counter.get()+1));
        let selections=Rc::new(Cell::new(0));let counter=selections.clone();ui.on_toggle_all(move ||counter.set(counter.get()+1));
        click(&ui,240.,108.);assert_eq!(reads.get(),0);
        ui.set_scope(1);click(&ui,240.,108.);assert_eq!(reads.get(),1);
        click(&ui,290.,108.);assert_eq!(selections.get(),1);
        let previews=Rc::new(Cell::new(0));let counter=previews.clone();ui.on_preview_status_clicked(move ||counter.set(counter.get()+1));
        click(&ui,100.,685.);assert_eq!(previews.get(),1);
        ui.set_web_running(true);ui.set_web_address("http://127.0.0.1:8686/".into());
        click(&ui,100.,685.);assert_eq!(previews.get(),2);
    }
    #[test]
    fn header_account_login_menu_busy_guard_and_status_independence() {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
        let ui = AppWindow::new().unwrap();
        window.set_size(slint::PhysicalSize::new(1040,700));
        let logins = Rc::new(Cell::new(0)); let count = logins.clone();
        ui.on_qr_login(move || count.set(count.get()+1));
        assert!(!ui.get_account_logged_in());
        click(&ui,875.,35.); assert_eq!(logins.get(),0);
        click(&ui,875.,132.); assert_eq!(logins.get(),1);
        ui.set_busy(true); click(&ui,875.,35.); assert_eq!(logins.get(),1);
        ui.set_busy(false); ui.set_account_logged_in(true);
        ui.set_account_name("一个很长的用户名用于验证账号入口的省略显示".into());
        ui.set_account_uid("123".into());
        ui.set_status_text("网页预览已启动".into()); ui.set_status_ok(true);
        assert!(ui.get_account_logged_in()); assert_eq!(ui.get_account_uid().as_str(),"123");
        click(&ui,875.,35.); assert_eq!(logins.get(),1,"a logged-in click opens its menu");
        click(&ui,800.,140.); assert_eq!(logins.get(),2,"menu offers account switching");
        ui.set_busy(true); click(&ui,875.,35.); click(&ui,800.,140.);
        assert_eq!(logins.get(),2,"tasks disable switching accounts");
    }
    #[test]
    fn message_dates_defaults_calendar_and_reset(){
        let window=MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
        let ui=AppWindow::new().unwrap();window.set_size(slint::PhysicalSize::new(1040,700));
        message_dates::install(&ui,&ui.as_weak());
        assert!(ui.get_message_from().is_empty());assert_eq!(ui.get_message_to().as_str(),chrono::Local::now().format("%Y-%m-%d").to_string());assert!(ui.get_message_date_auto_end());
        assert!(ui.invoke_message_calendar_accept(1,MessageCalendarDate{year:2026,month:2,day:28}));
        assert!(!ui.get_message_date_auto_end());
        message_dates::refresh_automatic_end(&ui);
        assert_eq!(ui.get_message_to().as_str(),"2026-02-28");
        ui.set_message_from("2024-02-29".into());let seed=ui.invoke_message_calendar_open(0);assert_eq!((seed.year,seed.month,seed.day),(2024,2,29));
        ui.invoke_message_calendar_accept(0,MessageCalendarDate{year:2026,month:3,day:1});assert_eq!(ui.get_message_from().as_str(),"2024-02-29");assert!(!ui.get_message_date_error().is_empty());
        ui.invoke_message_calendar_accept(0,MessageCalendarDate{year:2026,month:2,day:1});assert_eq!(ui.get_message_from().as_str(),"2026-02-01");assert!(ui.get_message_date_error().is_empty());
        ui.invoke_message_date_clear(1);message_dates::refresh_automatic_end(&ui);assert!(ui.get_message_to().is_empty());assert!(!ui.get_message_date_auto_end());
        let generation=ui.get_message_calendar_generation();message_dates::reset(&ui);assert!(ui.get_message_from().is_empty());assert!(ui.get_message_date_auto_end());assert_ne!(ui.get_message_calendar_generation(),generation);
    }
    #[test]
    fn group_confirmation_defaults_cancel_close_and_accept() {
        let window=MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
        let ui=AppWindow::new().unwrap();window.set_size(slint::PhysicalSize::new(1280,900));
        assert!(!ui.get_message_groups());assert!(!ui.get_message_groups_confirm());
        ui.set_message_groups_confirm(true);click(&ui,640.,440.);
        assert!(ui.get_message_groups_confirm(),"blank card area must not cancel");
        click(&ui,20.,500.);assert!(!ui.get_message_groups_confirm());assert!(!ui.get_message_groups());
        ui.set_message_groups_confirm(true);click(&ui,670.,510.);
        assert!(!ui.get_message_groups_confirm());assert!(!ui.get_message_groups());
        ui.set_message_groups_confirm(true);click(&ui,804.,390.);
        assert!(!ui.get_message_groups_confirm());assert!(!ui.get_message_groups());
        ui.set_message_groups_confirm(true);click(&ui,770.,510.);
        assert!(!ui.get_message_groups_confirm());assert!(ui.get_message_groups());
    }
    #[test]
    fn card_clicks_save_and_outside_close() {
        // Component events only: no native window, actual profile or network access.
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
        let ui = AppWindow::new().unwrap();
        window.set_size(slint::PhysicalSize::new(1280, 900));
        ui.set_settings_visible(true);
        ui.set_web_port_text("8686".into());
        ui.set_page_delay_text("1100".into());
        ui.set_page_delay_max_text("1100".into());
        ui.set_export_dir_text("C:/profile/exports".into());
        let closed = Rc::new(Cell::new(0));
        let c = closed.clone();
        ui.on_settings_close(move || c.set(c.get() + 1));
        let saved = Rc::new(Cell::new(0));
        let c = saved.clone();
        ui.on_settings_apply(move || c.set(c.get() + 1));
        click(&ui, 855., 450.); // blank inside the card
        assert_eq!(closed.get(), 0, "card blank area must consume clicks");
        click(&ui, 455., ui.get_settings_dialog_top()+109.); // port input
        assert_eq!(closed.get(), 0, "editing a setting must not close the card");
        click(&ui, 817., ui.get_settings_dialog_top()+ui.get_settings_dialog_height()-33.); // bottom-right Save button in the scrollable settings card
        assert_eq!(
            saved.get(),
            1,
            "Save button must fit inside the compact card"
        );
        assert_eq!(closed.get(), 0, "Save must not close the card");
        assert!(ui.get_settings_visible());
        click(&ui, 832., ui.get_settings_dialog_top()+34.);
        assert_eq!(closed.get(), 1, "header close button must close settings");
        click(&ui, 20., 450.);
        assert_eq!(closed.get(), 2, "outside overlay must also close settings");
    }

    #[test]
    fn independent_collection_ui_values_and_defaults() {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window))).unwrap();
        let ui = AppWindow::new().unwrap();
        let mut ranges = collection::defaults();
        ranges.get_mut(&1).unwrap().from = 3;
        ranges.get_mut(&11).unwrap().to = 7;
        ranges.get_mut(&17).unwrap().enabled = false;
        set_ui_ranges(&ui, &ranges);
        assert_eq!(ranges_from_ui(&ui).unwrap(), ranges);
        ui.set_range_article_from("9".into());
        ui.set_range_article_to("8".into());
        assert!(ranges_from_ui(&ui).is_err());
        ui.set_range_article_from("1".into());
        ui.set_range_article_to("".into());
        assert_eq!(ranges_from_ui(&ui).unwrap()[&12].to, 0);
        assert_eq!(ui.get_page_delay_text().as_str(), "1800");
        assert_eq!(ui.get_page_delay_max_text().as_str(), "2400");
    }

    #[test]
    fn history_import_ignores_stale_login_and_account_switch_isolates_data() {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window))).unwrap();
        let ui = AppWindow::new().unwrap();
        let weak = ui.as_weak();
        let root = tempfile::tempdir().unwrap();
        export::export_item_for(
            root.path(),
            &Video {
                aid: 42,
                owner_mid: 123,
                comments: vec![model::Comment {
                    rpid: 7,
                    ..Default::default()
                }],
                ..Default::default()
            },123,
        )
        .unwrap();
        let mut cfg = Cfg::default();
        cfg.export_dir = root.path().to_string_lossy().into_owned();
        cfg.sessdata = "fake-session".into();
        let shared = Arc::new(Shared::new(cfg));
        shared.account_uid.store(123, Ordering::SeqCst);
        load_history(&shared, &weak, 123, "stale-session");
        assert!(shared.doc.lock().unwrap().videos.is_empty());
        load_history(&shared, &weak, 123, "fake-session");
        assert_eq!(shared.doc.lock().unwrap().stats.comments, 1);
        load_history(&shared, &weak, 123, "fake-session");
        assert_eq!(shared.rows.lock().unwrap().len(), 1);
        set_account(&shared, &weak, 456);
        assert!(shared.doc.lock().unwrap().videos.is_empty());
        assert!(shared.rows.lock().unwrap().is_empty());
    }

    #[test]
    fn wait_modal_blocks_background_and_settings_until_background_wait_is_chosen() {
        let window=MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
        let ui=AppWindow::new().unwrap();window.set_size(slint::PhysicalSize::new(1280,900));
        let opened=Rc::new(Cell::new(0));let c=opened.clone();ui.on_settings_open(move||c.set(c.get()+1));
        let preview=Rc::new(Cell::new(0));let c=preview.clone();ui.on_preview_status_clicked(move||c.set(c.get()+1));
        let saved=Rc::new(Cell::new(0));let c=saved.clone();ui.on_settings_apply(move||c.set(c.get()+1));
        let weak=ui.as_weak();ui.on_verify_dismiss(move||{let w=weak.upgrade().unwrap();w.set_verify_background(true);w.set_verify_visible(false);});
        click(&ui,1218.,35.);assert_eq!(opened.get(),1);
        ui.set_verify_visible(true);ui.set_verify_waiting(true);ui.set_verify_secs(60);
        click(&ui,1218.,35.);click(&ui,100.,885.);click(&ui,640.,450.);
        assert_eq!(opened.get(),1);assert_eq!(preview.get(),0);assert!(ui.get_verify_visible());
        ui.set_settings_visible(true);
        let save_y=ui.get_settings_dialog_top()+ui.get_settings_dialog_height()-33.;
        click(&ui,817.,save_y);assert_eq!(saved.get(),0,"settings behind the modal must not receive clicks");
        click(&ui,792.,524.);assert!(!ui.get_verify_visible());assert!(ui.get_verify_background());assert!(ui.get_verify_waiting());
        click(&ui,817.,save_y);assert_eq!(saved.get(),1,"background wait restores page interaction");
        ui.set_settings_visible(false);click(&ui,1218.,35.);assert_eq!(opened.get(),2);
    }

    #[test]
    fn progress_shows_wait_time_even_when_dialog_is_hidden() {
        let window=MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window))).unwrap();
        let ui=AppWindow::new().unwrap();
        ui.set_progress_text("视频第 3 页".into());
        ui.set_verify_waiting(true);ui.set_verify_secs(60);
        assert_eq!(ui.get_progress_display_text().as_str(),"视频第 3 页 · 风控等待 60 秒");
        ui.set_verify_visible(false);ui.set_verify_background(true);ui.set_verify_secs(59);
        assert!(ui.get_progress_display_text().as_str().contains("59 秒"));
        ui.set_verify_secs(0);
        assert!(ui.get_progress_display_text().as_str().contains("正在检查恢复"));
        ui.set_verify_waiting(false);
        assert_eq!(ui.get_progress_display_text().as_str(),"视频第 3 页");
    }



    #[test]
    fn availability_and_bounds_render_consistently_for_all_five_types() {
        let window=MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window))).unwrap();
        let ui=AppWindow::new().unwrap();
        let mut session=range_ui::Session::new(&collection::defaults());
        range_ui::render(&ui,&session);
        for ty in collection::TYPES {assert_eq!(range_input(&ui,ty).unwrap(),(String::new(),String::new(),None));}
        for ty in collection::TYPES {
            let state=session.ranges.get_mut(&ty).unwrap();state.update_total(Some(65));state.edit("3","999");state.all();
        }
        range_ui::render(&ui,&session);
        for ty in collection::TYPES {assert_eq!(range_input(&ui,ty).unwrap(),("1".into(),"7".into(),Some(7)));}
        for state in session.ranges.values_mut(){state.update_total(Some(0));}
        range_ui::render(&ui,&session);
        assert!(!ui.get_range_video_enabled()&&!ui.get_range_audio_available());
        for ty in collection::TYPES {assert_eq!(range_input(&ui,ty).unwrap(),(String::new(),String::new(),Some(0)));}
    }

    #[test]
    fn local_reading_blocks_pause_stop_and_restores_control_on_exit() {
        let window=MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window))).unwrap();
        let ui=AppWindow::new().unwrap();let weak=ui.as_weak();
        let shared=Arc::new(Shared::new(Cfg::default()));
        *shared.checkpoint.lock().unwrap()=Some(checkpoint::Task::new(123,collection::defaults(),true,vec![]));
        {
            let _guard=LocalReadGuard::begin(&shared,&weak);
            assert!(shared.loading_local.load(Ordering::SeqCst));
            pause_collection(&shared,&weak);stop_collection(&shared,&weak);
            assert!(!shared.pause_requested.load(Ordering::SeqCst));assert!(!shared.cancel.load(Ordering::SeqCst));
            assert!(shared.checkpoint.lock().unwrap().is_some(),"reading must not delete collection checkpoints");
        }
        assert!(!shared.loading_local.load(Ordering::SeqCst));
        pause_collection(&shared,&weak);
        assert!(!shared.cancel.load(Ordering::SeqCst),"idle operations cannot be paused");
        let task=TaskGuard::begin(&shared,&weak,1).unwrap();
        pause_collection(&shared,&weak);
        assert!(shared.pause_requested.load(Ordering::SeqCst));assert!(shared.cancel.load(Ordering::SeqCst));
        drop(task);assert_eq!(shared.active_task.load(Ordering::SeqCst),0);assert!(!shared.busy.load(Ordering::SeqCst));
    }

    #[test]
    fn workspace_panels_share_bounds_across_sizes_dpi_and_task_states() {
        use slint::Model as _;
        let window=MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
        let ui=AppWindow::new().unwrap();ui.show().unwrap();
        ui.set_scope(1);
        ui.set_message_from("2026/11/".into());
        ui.set_message_date_error("日期尚未填写完成，请补齐年月日。".repeat(12).into());
        ui.set_log_text("22:00:00  很长的运行日志内容，应该仅在日志面板内部滚动\n".repeat(500).into());
        ui.set_message_ng_count(195);
        for (width,height,scale) in [(1200u32,792u32,1.0f32),(1040,700,1.0),(1200,792,1.25),(1040,700,1.5),(1280,900,2.0)] {
            ui.window().dispatch_event(WindowEvent::ScaleFactorChanged{scale_factor:scale});
            let physical_width=(width as f32*scale).round() as u32;
            let physical_height=(height as f32*scale).round() as u32;
            window.set_size(slint::PhysicalSize::new(physical_width,physical_height));
            let mut pixels = vec![slint::Rgb8Pixel::default(); physical_width as usize * physical_height as usize];
            for (active,busy,loading) in [(0,false,false),(1,true,false),(2,true,false),(3,true,false),(0,true,true)] {
                ui.set_active_task(active);ui.set_busy(busy);ui.set_loading_local(loading);
                ui.set_workspace(0);ui.window().request_redraw();
                window.draw_if_needed(|renderer|{renderer.render(&mut pixels,physical_width as usize);});
                let comments=ui.get_comments_panel_bounds().iter().collect::<Vec<_>>();
                let comment_text=ui.get_comments_text_bounds().iter().collect::<Vec<_>>();
                ui.set_workspace(1);ui.window().request_redraw();
                window.draw_if_needed(|renderer|{renderer.render(&mut pixels,physical_width as usize);});
                let messages=ui.get_messages_panel_bounds().iter().collect::<Vec<_>>();
                let message_text=ui.get_messages_text_bounds().iter().collect::<Vec<_>>();
                assert_eq!(comment_text.len(),6);assert_eq!(message_text.len(),6);
                for (a,b) in comment_text.iter().zip(&message_text){assert!((a-b).abs()<0.1,"workspace title baselines differ: {a} != {b}");}
                assert_eq!(comments.len(),28);assert_eq!(messages.len(),28);
                assert!((ui.get_comments_action_bottom_gap()-6.).abs()<0.1);
                assert!((ui.get_messages_action_bottom_gap()-6.).abs()<0.1);
                for (index,(a,b)) in comments.iter().zip(&messages).enumerate() {
                    assert!((a-b).abs()<0.1,"geometry {index} at {width}x{height}, DPI {scale}, task {active}: {a} != {b}");
                }
                assert!((comments[0]-16.).abs()<0.1);assert!((comments[1]-14.).abs()<0.1);
                assert!((comments[2]-372.).abs()<0.1);
                assert!((comments[4]-comments[0]-comments[2]-14.).abs()<0.1);
                assert!((comments[15]-123.).abs()<0.1);assert!((comments[19]-101.).abs()<0.1);
                assert!((comments[17]-comments[13]-comments[15]-8.).abs()<0.1);
                assert!((comments[21]-comments[17]-comments[19]-14.).abs()<0.1);
                assert!(comments[23]>=180.);
                assert!(comments[24..].iter().all(|r|(*r-14.).abs()<0.1));
                assert_eq!(ui.get_message_from().as_str(),"2026/11/");assert_eq!(ui.get_scope(),1);
                assert_eq!(ui.get_active_task(),active);
            }
        }
    }

    #[test]
    fn cached_message_workspace_does_not_read_source_files_or_change_task_state() {
        let window=MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window))).unwrap();
        let ui=AppWindow::new().unwrap();
        let temp=tempfile::tempdir().unwrap();let mut cfg=Cfg::default();cfg.export_dir=temp.path().to_string_lossy().into_owned();
        let shared=Arc::new(Shared::new(cfg));shared.account_uid.store(12,Ordering::SeqCst);
        {
            let mut state=shared.messages.lock().unwrap();state.doc=messages::Doc::new(12);state.root=messages::base(temp.path(),12);
            // If the cached path is accidentally reloaded or rescanned this invalid
            // backing file would fail parsing; the mounted projection must remain untouched.
            std::fs::create_dir_all(&state.root).unwrap();std::fs::write(state.root.join("likes.json"),b"broken archive").unwrap();
        }
        let(tx,rx)=std::sync::mpsc::channel();assert!(messages_ui::request_view(&shared,&tx));
        let uid=match rx.try_recv().unwrap(){Cmd::EnsureMessageView{uid}=>uid,_=>panic!("unexpected command")};
        shared.busy.store(true,Ordering::SeqCst);shared.active_task.store(2,Ordering::SeqCst);
        ui.set_message_count_likes("37".into());ui.set_message_ng_count(195);ui.set_message_has_resume(true);
        messages_ui::ensure_view(uid,&shared,&ui.as_weak());
        assert_eq!(ui.get_message_count_likes().as_str(),"37");assert_eq!(ui.get_message_ng_count(),195);assert!(ui.get_message_has_resume());
        assert_eq!(shared.message_view_pending.load(Ordering::SeqCst),0);
        assert!(shared.busy.load(Ordering::SeqCst));assert_eq!(shared.active_task.load(Ordering::SeqCst),2);
        // An old account request must neither load a file nor release a newer account's request.
        shared.account_uid.store(13,Ordering::SeqCst);assert!(messages_ui::request_view(&shared,&tx));
        messages_ui::ensure_view(12,&shared,&ui.as_weak());assert_eq!(shared.message_view_pending.load(Ordering::SeqCst),13);
        assert_eq!(shared.messages.lock().unwrap().doc.account_uid,"12");
    }
}

#[cfg(test)]
mod export_directory_tests {
    use super::*;
    #[test]
    fn default_is_below_profile_and_custom_is_preserved() {
        let mut cfg = Cfg::default();
        assert_eq!(
            PathBuf::from(export_dir_of(&cfg)),
            checkpoint::data_dir().unwrap().join("exports")
        );
        cfg.export_dir = "D:/chosen-output".into();
        assert_eq!(export_dir_of(&cfg), "D:/chosen-output");
    }
}

fn load_startup_archives(shared: &Arc<Shared>, ui: &Weak<AppWindow>, uid: u64, expected_cookie: &str) {
    let _loading = LocalReadGuard::begin(shared, ui);
    let (root, precache, workers) = {
        let cfg = shared.cfg.lock().unwrap();
        (PathBuf::from(export_dir_of(&cfg)), cfg.archive_precache, cfg.archive_workers())
    };
    let read_messages = || messages_ui::load_overview(shared, ui, uid);
    if !precache || !preview_disk::small_comment_archive(&root, uid) {
        let projection = preview_disk::desktop_projection(&root, uid);
        let cfg = shared.cfg.lock().unwrap();
        if cfg.sessdata != expected_cookie
            || shared.account_uid.load(Ordering::SeqCst) != uid
            || std::path::Path::new(&export_dir_of(&cfg)) != root.as_path()
        { return; }
        *shared.doc.lock().unwrap() = projection;
        sync_history_rows(shared);
        drop(cfg);
        refresh_rows(shared, ui);
        refresh_stats(shared, ui);
        restore_checkpoint(shared, ui, uid);
        if precache { read_messages(); }
        return;
    }
    let read_comments = || {
        load_history_with_workers(shared, ui, uid, expected_cookie, &root, workers.saturating_sub(1).max(1));
        if shared.account_uid.load(Ordering::SeqCst) == uid
            && shared.cfg.lock().unwrap().sessdata == expected_cookie
        {
            restore_checkpoint(shared, ui, uid);
        }
    };
    if workers > 1 {
        std::thread::scope(|scope| {
            let messages = scope.spawn(read_messages);
            read_comments();
            messages.join().expect("message reader panicked");
        });
    } else {
        read_comments();
        read_messages();
    }
}

#[cfg(test)]
fn load_history(shared: &Arc<Shared>, ui: &Weak<AppWindow>, uid: u64, expected_cookie: &str) {
    let root = PathBuf::from(export_dir_of(&shared.cfg.lock().unwrap()));
    load_history_with_workers(shared, ui, uid, expected_cookie, &root, local_read::workers());
}

fn load_history_with_workers(
    shared: &Arc<Shared>,
    ui: &Weak<AppWindow>,
    uid: u64,
    expected_cookie: &str,
    root: &std::path::Path,
    workers: usize,
) {
    let imported = export::load_user_exports_with_workers(root, uid, workers);
    // Hold the credential lock until the result is applied so a superseded login
    // cannot insert a different account's data during an account transition.
    let cfg = shared.cfg.lock().unwrap();
    if cfg.sessdata != expected_cookie
        || shared.account_uid.load(Ordering::SeqCst) != uid
        || std::path::Path::new(&export_dir_of(&cfg)) != root
    {
        return;
    }
    shared.doc.lock().unwrap().merge_videos(imported.doc.videos);
    sync_history_rows(shared);
    drop(cfg);
    for error in imported.errors {
        push_log(shared, ui, error);
    }
    push_log(
        shared,
        ui,
        format!(
            "历史导出：导入 {} 个文件，跳过 {} 个文件",
            imported.loaded, imported.skipped
        ),
    );
    refresh_rows(shared, ui);
    refresh_stats(shared, ui);
}

fn sync_history_rows(shared: &Arc<Shared>) {
    let videos: Vec<_> = shared.doc.lock().unwrap().videos.iter()
        .map(Video::without_comments).collect();
    let mut rows = shared.rows.lock().unwrap();
    let mut positions: std::collections::HashMap<_, _> = rows.iter().enumerate()
        .map(|(index, (video, _))| ((video.reply_type(), video.aid), index))
        .collect();
    for video in videos {
        let key = (video.reply_type(), video.aid);
        if let Some(&index) = positions.get(&key) {
            rows[index].0 = video;
        } else {
            positions.insert(key, rows.len());
            rows.push((video, false));
        }
    }
}

fn query_type_totals(
    shared: &Arc<Shared>,
    ui: &Weak<AppWindow>,
    client: &bili::Bili,
    types: &[u32],
    expected_cookie: &str,
    uid: u64,
    batch: u64,
) {
    for (index, &ty) in types.iter().enumerate() {
        if shared.cfg.lock().unwrap().sessdata != expected_cookie
            || shared.account_uid.load(Ordering::SeqCst) != uid
        {
            return;
        }
        let result = client.up_list_page(ty, 1, collection::PAGE_SIZE, 0);
        if shared.cfg.lock().unwrap().sessdata != expected_cookie
            || shared.account_uid.load(Ordering::SeqCst) != uid
        {
            return;
        }
        match result {
            Ok((_, total)) => {complete_range_read(shared,ui,batch,ty,Some(total));},
            Err(e) => {
                complete_range_read(shared,ui,batch,ty,None);
                push_log(
                    shared,
                    ui,
                    format!("{}总数获取失败：{}", checkpoint::label(ty), e),
                );
            }
        }
        if index + 1 < types.len() {
            std::thread::sleep(client.next_pause());
        }
    }
}

#[cfg(test)]
fn set_ui_ranges(w: &AppWindow, ranges: &collection::Ranges) {
    if let Some(r) = ranges.get(&1) {
        w.set_range_video_enabled(r.enabled);
        w.set_range_video_from(r.from.to_string().into());
        w.set_range_video_to(
            if r.to == 0 {
                String::new()
            } else {
                r.to.to_string()
            }
            .into(),
        );
    }
    if let Some(r) = ranges.get(&11) {
        w.set_range_dynamic_enabled(r.enabled);
        w.set_range_dynamic_from(r.from.to_string().into());
        w.set_range_dynamic_to(
            if r.to == 0 {
                String::new()
            } else {
                r.to.to_string()
            }
            .into(),
        );
    }
    if let Some(r) = ranges.get(&17) {
        w.set_range_repost_enabled(r.enabled);
        w.set_range_repost_from(r.from.to_string().into());
        w.set_range_repost_to(
            if r.to == 0 {
                String::new()
            } else {
                r.to.to_string()
            }
            .into(),
        );
    }
    if let Some(r) = ranges.get(&12) {
        w.set_range_article_enabled(r.enabled);
        w.set_range_article_from(r.from.to_string().into());
        w.set_range_article_to(
            if r.to == 0 {
                String::new()
            } else {
                r.to.to_string()
            }
            .into(),
        );
    }
    if let Some(r) = ranges.get(&14) {
        w.set_range_audio_enabled(r.enabled);
        w.set_range_audio_from(r.from.to_string().into());
        w.set_range_audio_to(
            if r.to == 0 {
                String::new()
            } else {
                r.to.to_string()
            }
            .into(),
        );
    }
}
fn ranges_from_ui(w: &AppWindow) -> Result<collection::Ranges> {
    let mut ranges = collection::Ranges::new();
    ranges.insert(
        1,
        collection::parse(
            w.get_range_video_enabled(),
            w.get_range_video_from().as_str(),
            w.get_range_video_to().as_str(),
        )
        .map_err(|e| anyhow::anyhow!("{}：{}", checkpoint::label(1), e))?,
    );
    ranges.insert(
        11,
        collection::parse(
            w.get_range_dynamic_enabled(),
            w.get_range_dynamic_from().as_str(),
            w.get_range_dynamic_to().as_str(),
        )
        .map_err(|e| anyhow::anyhow!("{}：{}", checkpoint::label(11), e))?,
    );
    ranges.insert(
        17,
        collection::parse(
            w.get_range_repost_enabled(),
            w.get_range_repost_from().as_str(),
            w.get_range_repost_to().as_str(),
        )
        .map_err(|e| anyhow::anyhow!("{}：{}", checkpoint::label(17), e))?,
    );
    ranges.insert(
        12,
        collection::parse(
            w.get_range_article_enabled(),
            w.get_range_article_from().as_str(),
            w.get_range_article_to().as_str(),
        )
        .map_err(|e| anyhow::anyhow!("{}：{}", checkpoint::label(12), e))?,
    );
    ranges.insert(
        14,
        collection::parse(
            w.get_range_audio_enabled(),
            w.get_range_audio_from().as_str(),
            w.get_range_audio_to().as_str(),
        )
        .map_err(|e| anyhow::anyhow!("{}：{}", checkpoint::label(14), e))?,
    );
    Ok(ranges)
}
fn render_range_session(shared:&Arc<Shared>,ui:&Weak<AppWindow>) {
    let shared=shared.clone();
    emit(ui,move|w|range_ui::render(w,&shared.range_session.lock().unwrap()));
}
fn complete_range_read(shared:&Arc<Shared>,ui:&Weak<AppWindow>,batch:u64,ty:u32,total:Option<u64>) {
    let changed=shared.range_session.lock().unwrap().complete(batch,ty,total);
    if changed {render_range_session(shared,ui);}
}
fn set_type_total(shared:&Arc<Shared>,ui:&Weak<AppWindow>,ty:u32,total:Option<u64>) {
    if let Some(state)=shared.range_session.lock().unwrap().ranges.get_mut(&ty){state.update_total(total);}
    render_range_session(shared,ui);
}

fn range_input(w:&AppWindow,ty:u32)->Option<(String,String,Option<u64>)> {
    match ty {
        1=>Some((w.get_range_video_from().to_string(),w.get_range_video_to().to_string(),w.get_range_video_limit().parse().ok())),
        11=>Some((w.get_range_dynamic_from().to_string(),w.get_range_dynamic_to().to_string(),w.get_range_dynamic_limit().parse().ok())),
        17=>Some((w.get_range_repost_from().to_string(),w.get_range_repost_to().to_string(),w.get_range_repost_limit().parse().ok())),
        12=>Some((w.get_range_article_from().to_string(),w.get_range_article_to().to_string(),w.get_range_article_limit().parse().ok())),
        14=>Some((w.get_range_audio_from().to_string(),w.get_range_audio_to().to_string(),w.get_range_audio_limit().parse().ok())),
        _=>None,
    }
}
fn merge_online_metadata(shared: &Arc<Shared>, video: &Video) {
    let mut doc = shared.doc.lock().unwrap();
    if let Some(old) = doc
        .videos
        .iter_mut()
        .find(|v| checkpoint::key(v) == checkpoint::key(video))
    {
        for (dst, src) in [
            (&mut old.title, &video.title),
            (&mut old.bvid, &video.bvid),
            (&mut old.cover, &video.cover),
            (&mut old.url, &video.url),
            (&mut old.owner_name, &video.owner_name),
        ] {
            if !src.is_empty() {
                *dst = src.clone();
            }
        }
        if video.owner_mid > 0 {
            old.owner_mid = video.owner_mid;
        }
        if video.publish_time > 0 {
            old.publish_time = video.publish_time;
            old.publish_time_text = video.publish_time_text.clone();
        }
        if !video.status.is_empty() && video.status != "unknown" {
            old.status = video.status.clone();
            old.status_text = video.status_text.clone();
        }
        old.comment_total = video.comment_total;
    }
    doc.recount();
}

#[cfg(test)]
mod historical_data_tests {
    use super::*;
    #[test]
    fn online_metadata_preserves_comments_without_a_required_online_result() {
        let shared = Arc::new(Shared::new(Cfg::default()));
        shared.doc.lock().unwrap().merge_video(Video {
            aid: 1,
            owner_mid: 123,
            title: "old".into(),
            comments: vec![model::Comment {
                rpid: 7,
                ..Default::default()
            }],
            ..Default::default()
        });
        merge_online_metadata(
            &shared,
            &Video {
                aid: 1,
                owner_mid: 123,
                title: "new".into(),
                ..Default::default()
            },
        );
        sync_history_rows(&shared);
        assert_eq!(shared.rows.lock().unwrap()[0].0.title, "new");
        assert!(!shared.rows.lock().unwrap()[0].1);
        assert_eq!(shared.doc.lock().unwrap().videos[0].comments[0].rpid, 7);
        // No online result is required for the locally imported data to remain usable.
        assert_eq!(shared.doc.lock().unwrap().stats.comments, 1);
        let cfg = Cfg::default();
        assert_eq!((cfg.interval_ms, cfg.interval_max_ms), (1800, 2400));
    }
}


struct LocalReadGuard {
    shared: Arc<Shared>,
    ui: Weak<AppWindow>,
}
impl LocalReadGuard {
    fn messages(shared:&Arc<Shared>,ui:&Weak<AppWindow>)->Self {
        let guard=Self::begin(shared,ui);
        emit(ui,|w|w.set_progress_text("正在处理本地消息归档…".into()));guard
    }
    fn begin(shared:&Arc<Shared>,ui:&Weak<AppWindow>)->Self {
        shared.loading_local.store(true,Ordering::SeqCst);
        emit(ui,|w|{w.set_loading_local(true);w.set_progress_text("正在读取本地稿件…".into());});
        Self{shared:shared.clone(),ui:ui.clone()}
    }
}
impl Drop for LocalReadGuard {
    fn drop(&mut self) {
        self.shared.loading_local.store(false,Ordering::SeqCst);
        emit(&self.ui,|w|w.set_loading_local(false));
    }
}

fn pause_collection(shared:&Arc<Shared>,ui:&Weak<AppWindow>) {
    if shared.loading_local.load(Ordering::SeqCst) || shared.active_task.load(Ordering::SeqCst)==0 {return;}
    shared.pause_requested.store(true,Ordering::SeqCst);
    shared.cancel.store(true,Ordering::SeqCst);
    if shared.active_task.load(Ordering::SeqCst) == 1 {
        set_foot(ui, "正在暂停，保留进度…".into());
    }
    emit(ui,|w|{w.set_progress_text("正在暂停，保留进度…".into());});
}
fn stop_collection(shared:&Arc<Shared>,ui:&Weak<AppWindow>) {
    if shared.loading_local.load(Ordering::SeqCst) || shared.active_task.load(Ordering::SeqCst)==0 {return;}
    shared.pause_requested.store(false,Ordering::SeqCst);
    shared.cancel.store(true,Ordering::SeqCst);
    if !shared.message_active.load(Ordering::SeqCst) {clear_checkpoint(shared,ui);}
    if shared.active_task.load(Ordering::SeqCst) == 1 {
        set_foot(ui, "正在结束…".into());
    }
    emit(ui,|w|{w.set_progress_text("正在结束…".into());});
}
