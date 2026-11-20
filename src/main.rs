#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bili;
mod checkpoint;
mod collection;
mod export;
mod model;
mod qrlogin;
mod secure;
mod web;

use anyhow::Result;
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
const PREVIEW_CHARS: usize = 5200;

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
    last_dir: String,
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
    /// 采集完成后自动导出（重复采集与已有文件增量合并）
    auto_export: bool,
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
            last_dir: dirs::desktop_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default(),
            include_replies: true,
            include_dynamics: true,
            ranges: collection::defaults(),
            interval_ms: 1800,
            interval_max_ms: 2400,
            proxy: String::new(),
            web_port: 8686,
            export_dir: String::new(),
            auto_export: false,
        }
    }
}

impl Cfg {
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
                    if let Some(s) = v["last_dir"].as_str() {
                        cfg.last_dir = s.to_string();
                    }
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
                    if let Some(b) = v["auto_export"].as_bool() {
                        cfg.auto_export = b;
                    }
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
            "last_dir": self.last_dir,
            "include_replies": self.include_replies,
            "include_dynamics": self.include_dynamics,
            "collection_ranges": self.ranges,
            "interval_ms": self.interval_ms,
            "interval_max_ms": self.interval_max_ms,
            "proxy": self.proxy,
            "web_port": self.web_port,
            "export_dir": self.export_dir,
            "auto_export": self.auto_export,
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
    rows: Mutex<Vec<(Video, bool)>>,
    cancel: AtomicBool,
    /// 停止意图：true=暂停（保留断点，下次「继续采集」从这里接上）；false=结束（清断点）
    pause_requested: AtomicBool,
    busy: AtomicBool,
    /// 扫码登录的「代次」：每次开新码 +1，旧线程发现代次变了就自行退出
    qr_gen: AtomicUsize,
    cfg: Mutex<Cfg>,
    account_uid: std::sync::atomic::AtomicU64,
    checkpoint: Mutex<Option<checkpoint::Task>>,
    log: Mutex<String>,
    last_export: Mutex<Option<PathBuf>>,
}

impl Shared {
    fn new(cfg: Cfg) -> Self {
        Self {
            doc: Arc::new(Mutex::new(ArchiveDoc::default())),
            rows: Mutex::new(Vec::new()),
            cancel: AtomicBool::new(false),
            pause_requested: AtomicBool::new(false),
            busy: AtomicBool::new(false),
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
    ReadArchives,
    AddVideos(Vec<String>),
    Start,
    CrawlUp,
    QueryTotals(Vec<u32>),
    SplitImport(Vec<PathBuf>),
    Import(Vec<PathBuf>),
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

/// 把配置里的断点状态同步到界面（暂停后显示「继续采集」与提示）
fn refresh_resume(shared: &Arc<Shared>, ui: &Weak<AppWindow>) {
    let task = shared.checkpoint.lock().unwrap();
    let has = task.is_some();
    let text = task.as_ref().map(|t| t.summary()).unwrap_or_default();
    emit(ui, move |w| {
        w.set_has_resume(has);
        w.set_resume_text(text.into());
    });
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
                            let _ = tx.send(Cmd::ReadArchives);
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
    let list: Vec<VideoRow> = rows
        .iter()
        .enumerate()
        .map(|(i, (v, checked))| VideoRow {
            index: i as i32,
            bvid: v.bvid.clone().into(),
            title: v.title.clone().into(),
            meta: {
                let id = if v.bvid.is_empty() {
                    format!("av{}", v.aid)
                } else {
                    v.bvid.clone()
                };
                let time = if v.publish_time_text.is_empty() {
                    "未知时间".to_string()
                } else {
                    v.publish_time_text.clone()
                };
                if v.comment_total > 0 {
                    format!("{} · {} 评论 · {}", id, human_count(v.comment_total), time)
                } else {
                    format!("{} · {}", id, time)
                }
            }
            .into(),
            checked: *checked,
        })
        .collect();
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
    let preview = export::to_json(&doc).unwrap_or_default();
    let preview: String = preview.chars().take(PREVIEW_CHARS).collect();
    let has = stats.comments > 0;
    drop(doc);
    emit(ui, move |w| {
        w.set_stat_videos(stats.videos as i32);
        w.set_stat_dynamics(stats.dynamics as i32);
        w.set_stat_comments(stats.comments as i32);
        w.set_stat_replies(stats.replies as i32);
        w.set_stat_likes(human_count(stats.likes).into());
        w.set_preview_text(preview.into());
        w.set_has_data(has);
    });
}

fn default_export_name() -> String {
    format!(
        "bili-comments-{}.json",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    )
}

/// 自动导出的固定文件名：同名才能做增量合并
fn export_dir_of(cfg: &Cfg) -> String {
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
    stop: Arc<AtomicBool>,
}

fn web_running(ui: &Weak<AppWindow>, running: bool) {
    emit(ui, move |w| w.set_web_running(running));
}

/// 启动服务；open_browser 控制是否顺手打开浏览器，require_data 用于「打开网页」入口
fn ensure_web(
    shared: &Arc<Shared>,
    ui: &Weak<AppWindow>,
    cache: &Arc<Mutex<Option<WebHandle>>>,
    port: u16,
    open_browser: bool,
    require_data: bool,
) {
    if let Some(url) = cache.lock().unwrap().as_ref().map(|h| h.url.clone()) {
        if open_browser {
            push_log(shared, ui, format!("预览已在运行：{}", url));
            let _ = webbrowser::open(&url);
        }
        return;
    }
    if require_data && shared.doc.lock().unwrap().stats.videos == 0 {
        push_log(shared, ui, "还没有数据，先采集或导入再预览");
        set_foot(ui, "还没有数据".into());
        return;
    }
    let doc = shared.doc.clone();
    let stop = Arc::new(AtomicBool::new(false));
    match web::spawn_with_stop(doc, port, stop.clone()) {
        Ok(port) => {
            let url = format!("http://127.0.0.1:{}/", port);
            *cache.lock().unwrap() = Some(WebHandle {
                url: url.clone(),
                stop,
            });
            push_log(
                shared,
                ui,
                format!("网页服务已启动：{}（关掉程序即停）", url),
            );
            set_status(ui, "网页预览已启动".into(), true);
            let address = url.clone();
            emit(ui, move |w| w.set_web_address(address.into()));
            web_running(ui, true);
            if open_browser {
                let _ = webbrowser::open(&url);
            }
        }
        Err(e) => {
            port_alert(ui, e.clone());
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
            h.stop.store(true, Ordering::Relaxed);
            // 监听线程最多 25ms 后醒来退出并释放端口，等它一下再允许重新启动
            std::thread::sleep(Duration::from_millis(40));
            push_log(shared, ui, format!("网页服务已停止：{}", h.url));
            set_status(ui, "网页服务已停止".into(), false);
            web_running(ui, false);
        }
        None => push_log(shared, ui, "网页服务本来就没在运行"),
    }
}

/// Export only the current task's objects, one stable archive per author/type/ID.
fn auto_export(shared: &Arc<Shared>, ui: &Weak<AppWindow>, videos: &[Video]) {
    let (enabled, dir) = {
        let cfg = shared.cfg.lock().unwrap();
        (cfg.auto_export, export_dir_of(&cfg))
    };
    if !enabled {
        return;
    }
    let mut ok = 0;
    let mut failed = 0;
    for video in videos {
        match export::export_item(std::path::Path::new(&dir), video) {
            Ok(_) => ok += 1,
            Err(e) => {
                failed += 1;
                push_log(
                    shared,
                    ui,
                    format!("稿件 {} 自动导出失败：{}", video.aid, e),
                );
            }
        }
    }
    if ok > 0 {
        *shared.last_export.lock().unwrap() = Some(PathBuf::from(&dir));
    }
    let message = format!("自动导出：成功 {}，失败 {} · {}", ok, failed, dir);
    push_log(shared, ui, &message);
    set_foot(ui, message);
}

fn worker(rx: Receiver<Cmd>, shared: Arc<Shared>, ui: Weak<AppWindow>) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            Cmd::ReadArchives => {
                let cookie = shared.cfg.lock().unwrap().sessdata.clone();
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
                    Ok((true, uname, mid)) => {
                        if shared.cfg.lock().unwrap().sessdata != expected_cookie {
                            set_busy(&shared, &ui, false);
                            continue;
                        }
                        set_account(&shared, &ui, mid);
                        set_status(&ui, format!("已连接 · {}", uname), true);
                        let who = uname.clone();
                        emit(&ui, move |w| {
                            w.set_account_text(format!("已登录：{}（UID {}）", uname, mid).into())
                        });
                        push_log(&shared, &ui, format!("账号校验通过：{}", who));
                    }
                    Ok((false, _, _)) => {
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
                load_history(&shared, &ui, uid, &expected_cookie);
                if shared.cfg.lock().unwrap().sessdata != expected_cookie {
                    set_busy(&shared, &ui, false);
                    continue;
                }
                restore_checkpoint(&shared, &ui, uid);

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
                            push_log(&shared, &ui, format!("读取稿件第 {} 页，获得 {} 条", pn, n));
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
                push_log(
                    &shared,
                    &ui,
                    format!(
                        "共 {} 个稿件，勾选后点「开始采集」",
                        shared.rows.lock().unwrap().len()
                    ),
                );
                set_progress(&ui, 1.0, format!("稿件 {}", total));
                set_foot(&ui, "稿件已载入，勾选后开始采集".into());

                let enabled = shared
                    .cfg
                    .lock()
                    .unwrap()
                    .ranges
                    .iter()
                    .filter_map(|(&ty, r)| r.enabled.then_some(ty))
                    .collect::<Vec<_>>();
                query_type_totals(&shared, &ui, &client, &enabled, &expected_cookie, uid);
                set_busy(&shared, &ui, false);
            }

            Cmd::QueryTotals(types) => {
                let expected = shared.cfg.lock().unwrap().sessdata.clone();
                let uid = shared.account_uid.load(Ordering::SeqCst);
                if uid == 0 || expected.is_empty() {
                    continue;
                }
                match make_client(&shared) {
                    Ok(client) => query_type_totals(&shared, &ui, &client, &types, &expected, uid),
                    Err(e) => push_log(&shared, &ui, format!("获取类型总数失败：{}", e)),
                }
            }
            Cmd::SplitImport(files) => {
                set_busy(&shared, &ui, true);
                let root = {
                    let cfg = shared.cfg.lock().unwrap();
                    export_dir_of(&cfg)
                };
                let report = export::split_import(&files, std::path::Path::new(&root));
                for error in report.errors {
                    push_log(&shared, &ui, error);
                }
                let message = format!(
                    "导入拆解：保存 {} 个稿件，失败/跳过 {} 项 · {}",
                    report.saved, report.failed, root
                );
                if report.saved > 0 {
                    *shared.last_export.lock().unwrap() = Some(PathBuf::from(&root));
                }
                push_log(&shared, &ui, &message);
                set_foot(&ui, message);
                set_busy(&shared, &ui, false);
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
                    set_foot(&ui, format!("已加入 {} 个稿件", added));
                }
                set_busy(&shared, &ui, false);
            }

            Cmd::Start => {
                let targets: Vec<Video> = shared
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
                let cfg = shared.cfg.lock().unwrap().clone();
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
                if let Err(e) = task.save() {
                    push_log(&shared, &ui, format!("无法保存采集进度：{}", e));
                    continue;
                }
                {
                    let mut rows = shared.rows.lock().unwrap();
                    for target in &task.targets {
                        if !rows
                            .iter()
                            .any(|(v, _)| checkpoint::key(v) == checkpoint::key(target))
                        {
                            rows.push((target.clone(), true));
                        }
                    }
                }
                active_task(&shared, &ui, &task);
                shared.cancel.store(false, Ordering::SeqCst);
                shared.pause_requested.store(false, Ordering::SeqCst);
                set_busy(&shared, &ui, true);
                let mut failed = false;
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
                    push_log(&shared, &ui, format!("采集：{}", video.display_title()));
                    let result = client.comments_of_resume(
                        &mut video,
                        cfg.include_replies,
                        &shared.cancel,
                        &mut cursor,
                        |v, cursor| {
                            if shared.cfg.lock().unwrap().sessdata != cfg.sessdata {
                                anyhow::bail!("登录账号已变更，原任务已保留");
                            }
                            task.targets[index] = v.clone();
                            task.cursors.insert(key.clone(), cursor.clone());
                            task.save()?;
                            active_task(&shared, &ui, &task);
                            {
                                shared.doc.lock().unwrap().merge_video(v.clone());
                            }
                            set_progress(
                                &ui,
                                index as f32 / task.targets.len().max(1) as f32,
                                format!(
                                    "{}/{} · 已抓 {} 条评论",
                                    index + 1,
                                    task.targets.len(),
                                    v.comments.len()
                                ),
                            );
                            Ok(())
                        },
                    );
                    task.targets[index] = video.clone();
                    task.cursors.insert(key.clone(), cursor);
                    shared.doc.lock().unwrap().merge_video(video);
                    match result {
                        Ok(()) => {
                            task.done.push(key);
                            if let Err(e) = task.save() {
                                push_log(&shared, &ui, format!("保存采集进度失败：{}", e));
                                failed = true;
                                break;
                            }
                        }
                        Err(e) => {
                            if !shared.cancel.load(Ordering::SeqCst) {
                                push_log(&shared, &ui, format!("采集失败，进度已保留：{}", e));
                                failed = true;
                            }
                            break;
                        }
                    }
                    refresh_stats(&shared, &ui);
                }
                let stopped = shared.cancel.load(Ordering::SeqCst)
                    && !shared.pause_requested.load(Ordering::SeqCst);
                let complete = task.complete();
                if complete || stopped {
                    clear_checkpoint(&shared, &ui);
                } else {
                    active_task(&shared, &ui, &task);
                }
                let message = if complete {
                    "采集完成"
                } else if stopped {
                    "已结束"
                } else if failed {
                    "采集失败，进度已保留"
                } else {
                    "已暂停，进度已保留"
                };
                set_busy(&shared, &ui, false);
                refresh_stats(&shared, &ui);
                set_progress(&ui, if complete { 1.0 } else { 0.0 }, message.into());
                push_log(&shared, &ui, message);
                set_foot(&ui, message.into());
                auto_export(&shared, &ui, &task.targets);
            }

            Cmd::CrawlUp => {
                let cfg = shared.cfg.lock().unwrap().clone();
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

                shared.cancel.store(false, Ordering::SeqCst);
                shared.pause_requested.store(false, Ordering::SeqCst);
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
                if let Err(e) = task.save() {
                    push_log(&shared, &ui, format!("无法保存采集进度：{}", e));
                    set_busy(&shared, &ui, false);
                    continue;
                }
                active_task(&shared, &ui, &task);
                let pace_min = cfg.interval_ms.clamp(200, 10000) as u64;
                let pace_max = cfg.interval_max_ms.clamp(200, 10000) as u64;
                let mut aborted = false;
                for stage_index in 0..task.stages.len() {
                    while !task.stages[stage_index].done {
                        if shared.cancel.load(Ordering::SeqCst) {
                            break;
                        }
                        if shared.cfg.lock().unwrap().sessdata != cfg.sessdata {
                            aborted = true;
                            break;
                        }
                        let ty = task.stages[stage_index].ty;
                        let page = task.stages[stage_index].page;
                        let label = checkpoint::label(ty);
                        set_progress(
                            &ui,
                            stage_index as f32 / task.stages.len().max(1) as f32,
                            format!("{}第 {} 页", label, page),
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
                                set_type_total(&ui, ty, Some(total));
                                task.accept(ty, page, page_rows, total);
                                if let Err(e) = task.save() {
                                    push_log(
                                        &shared,
                                        &ui,
                                        format!("保存采集进度失败，已停止：{}", e),
                                    );
                                    aborted = true;
                                    break;
                                }
                                active_task(&shared, &ui, &task);
                                let partial = bili::rows_to_videos(
                                    &task.rows,
                                    cfg.include_replies,
                                    self_mid,
                                    &self_name,
                                );
                                {
                                    let mut doc = shared.doc.lock().unwrap();
                                    for video in partial {
                                        doc.merge_video(video);
                                    }
                                }
                                refresh_stats(&shared, &ui);
                            }
                            Err(e) => {
                                if !shared.cancel.load(Ordering::SeqCst) {
                                    aborted = true;
                                    push_log(
                                        &shared,
                                        &ui,
                                        format!("{}第 {} 页失败，进度已保留：{}", label, page, e),
                                    );
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
                let rows = task.rows.clone();
                if rows.is_empty() {
                    if task.complete()
                        || (shared.cancel.load(Ordering::SeqCst)
                            && !shared.pause_requested.load(Ordering::SeqCst))
                    {
                        clear_checkpoint(&shared, &ui);
                    }
                    set_busy(&shared, &ui, false);
                    let message = if task.complete() {
                        "采集完成，暂无评论"
                    } else {
                        "采集未完成，进度已保留"
                    };
                    push_log(&shared, &ui, message);
                    set_foot(&ui, message.into());
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
                let message = if task.complete() {
                    "采集完成"
                } else if stopped {
                    "已结束"
                } else if aborted {
                    "采集失败，进度已保存"
                } else {
                    "已暂停，进度已保存"
                };
                set_progress(&ui, if task.complete() { 1.0 } else { 0.0 }, message.into());
                set_status(&ui, message.into(), task.complete());
                push_log(&shared, &ui, message);
                set_foot(
                    &ui,
                    format!(
                        "{} · {} 个稿件 / {} 条评论 / {} 条回复",
                        message, stats.videos, stats.comments, stats.replies
                    ),
                );
                auto_export(&shared, &ui, &exported);
            }

            Cmd::Import(files) => {
                set_busy(&shared, &ui, true);
                let mut ok = 0;
                for (i, path) in files.iter().enumerate() {
                    set_progress(
                        &ui,
                        (i + 1) as f32 / files.len() as f32,
                        format!("导入 {}/{}", i + 1, files.len()),
                    );
                    let name = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    match std::fs::read_to_string(path) {
                        Ok(text) => match export::import_json(&text, &name) {
                            Ok(videos) => {
                                let mut doc = shared.doc.lock().unwrap();
                                for v in videos {
                                    doc.merge_video(v);
                                }
                                push_log(&shared, &ui, format!("导入：{}", name));
                                ok += 1;
                            }
                            Err(e) => push_log(&shared, &ui, format!("{}：{}", name, e)),
                        },
                        Err(e) => push_log(&shared, &ui, format!("{}：{}", name, e)),
                    }
                }
                refresh_stats(&shared, &ui);
                let stats = shared.doc.lock().unwrap().stats.clone();
                push_log(
                    &shared,
                    &ui,
                    format!("导入完成 {} 个文件，现有 {} 条评论", ok, stats.comments),
                );
                set_foot(&ui, format!("现有 {} 条评论", stats.comments));
                set_busy(&shared, &ui, false);
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
                        let dir = path
                            .parent()
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let mut cfg = shared.cfg.lock().unwrap();
                        cfg.last_dir = dir;
                        cfg.save_reported();
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
    match args.first().map(|s| s.as_str()) {
        Some("serve") | Some("web") => return Some(cli_serve(&args[1..])),
        Some("login") => return Some(cli_login(&args[1..])),
        Some("crawl") => {}
        Some("-h") | Some("--help") | Some("help") => {
            print_help();
            return Some(0);
        }
        _ => return None,
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
        eprintln!("  bili-comment-hub crawl BV1xx411c7mD [BV...] [--out 文件] [--replies]");
        eprintln!("  bili-comment-hub crawl --all [--out 文件] [--from 1] [--to 140] [--merge] [--cookie SESSDATA]");
        eprintln!("  bili-comment-hub serve --file out.json [--port 8686] [--open]");
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
    println!("BiliCommentHub — 把创作中心评论管理的评论整合成 JSON");
    println!();
    println!("用法：");
    println!("  bili-comment-hub                                    打开图形界面");
    println!(
        "  bili-comment-hub login [--no-save]                  扫码登录并写入配置（终端画二维码）"
    );
    println!("  bili-comment-hub crawl BV1xx411c7mD [BV...] [--out 文件] [--replies]");
    println!("  bili-comment-hub crawl --all [--out 文件] [--from 1] [--to 140] [--merge] [--cookie SESSDATA]");
    println!("  bili-comment-hub serve --file out.json [--port 8686] [--open]");
    println!();
    println!("serve 会在本机起一个网页，把 JSON 列成可浏览的界面：");
    println!("  /              网页列表");
    println!("  /api/data.json 完整数据（JSON）");
    println!("  /api/videos    稿件摘要列表");
    println!("  /api/video?aid=123  单个稿件");
    println!("  /api/comments?q=关键词  跨稿件搜索评论（含楼中楼）");
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
        eprintln!("用法: bili-comment-hub serve --file out.json [--port 8686] [--open]");
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
            let url = format!("http://127.0.0.1:{}/", p);
            println!("网页已启动: {}", url);
            println!("  数据接口: {}api/data.json", url);
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
    // 代理策略要在任何请求之前定下来：配置里没写就直连，
    // 不跟随环境变量里那些常常已经失效的 http_proxy / https_proxy
    bili::set_proxy(&Cfg::load().proxy);
    if let Some(code) = run_cli() {
        std::process::exit(code);
    }

    let cfg = Cfg::load();
    let ui = AppWindow::new()?;

    ui.set_sessdata(cfg.sessdata.clone().into());
    ui.set_include_replies(cfg.include_replies);
    set_ui_ranges(&ui, &cfg.ranges);
    ui.set_foot_text("就绪 · 扫码登录，或直接粘贴 BV 号".into());
    ui.set_web_port_text(cfg.web_port.to_string().into());
    ui.set_export_dir_text(export_dir_of(&cfg).into());
    ui.set_auto_export(cfg.auto_export);
    ui.set_page_delay_text(cfg.interval_ms.to_string().into());
    ui.set_page_delay_max_text(cfg.interval_max_ms.to_string().into());

    let shared = Arc::new(Shared::new(cfg));
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

    // Saved credentials use the same validation/listing pipeline as a new login.
    if !shared.cfg.lock().unwrap().sessdata.trim().is_empty() {
        let _ = tx.send(Cmd::ReadArchives);
    }

    // ---- 读取稿件 ----
    {
        let tx = tx.clone();
        let shared = shared.clone();
        let weak = weak.clone();
        ui.on_read_archives(move |sessdata: SharedString| {
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
                push_log(&shared, &weak, "请先扫码登录，再试");
                return;
            }
            push_log(&shared, &weak, "正在读取创作中心稿件列表…");
            let _ = tx.send(Cmd::ReadArchives);
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
            if shared.busy.load(Ordering::SeqCst) {
                return;
            }
            let Some(w) = weak.upgrade() else {
                return;
            };
            let scope = w.get_scope();
            let mut cfg = shared.cfg.lock().unwrap();
            if scope == 0 {
                let ranges = match ranges_from_ui(&w) {
                    Ok(r) => r,
                    Err(e) => {
                        drop(cfg);
                        port_alert(&weak, e.to_string());
                        return;
                    }
                };
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
            // 暂停：停在当前批，保留断点，下次「继续采集」从这里接上
            shared.pause_requested.store(true, Ordering::SeqCst);
            shared.cancel.store(true, Ordering::SeqCst);
            emit(&weak, |w| w.set_progress_text("正在暂停，保留进度…".into()));
        });
    }

    {
        let shared = shared.clone();
        let weak = weak.clone();
        ui.on_stop(move || {
            // 结束：停在当前批并清除断点，下次「开始采集」从头来
            shared.pause_requested.store(false, Ordering::SeqCst);
            shared.cancel.store(true, Ordering::SeqCst);
            clear_checkpoint(&shared, &weak);
            emit(&weak, |w| w.set_progress_text("正在结束…".into()));
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
            push_log(&shared, &weak, "已清除断点，下次将从头开始采集");
        });
    }

    // ---- 导出 ----
    {
        let tx = tx.clone();
        let shared = shared.clone();
        let weak = weak.clone();
        ui.on_export_clicked(move || {
            // 优先用设置里的「导出目录」，没设过才回退到上次保存的位置
            let dir = {
                let cfg = shared.cfg.lock().unwrap();
                if cfg.export_dir.trim().is_empty() {
                    cfg.last_dir.clone()
                } else {
                    export_dir_of(&cfg)
                }
            };
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
                {
                    let mut cfg = shared.cfg.lock().unwrap();
                    if let Some(d) = path.parent() {
                        cfg.last_dir = d.to_string_lossy().to_string();
                    }
                    cfg.save_reported();
                }
                let _ = tx.send(Cmd::Export { path });
            }
            let _ = &weak;
        });
    }

    // ---- 网页预览（入口：右上角状态胶囊 / 设置里的「打开网页」） ----
    // 已经起过的服务地址，重复点击只是再打开一次浏览器，不重复占用端口
    let web_url: Arc<Mutex<Option<WebHandle>>> = Arc::new(Mutex::new(None));
    {
        let shared = shared.clone();
        let weak = weak.clone();
        let web_url = web_url.clone();
        ui.on_open_web(move || {
            let port = shared.cfg.lock().unwrap().web_port;
            ensure_web(&shared, &weak, &web_url, port, true, true);
        });
    }

    {
        let shared = shared.clone();
        let weak = weak.clone();
        let web_url = web_url.clone();
        ui.on_settings_open_web(move || {
            let port = shared.cfg.lock().unwrap().web_port;
            ensure_web(&shared, &weak, &web_url, port, true, true);
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
            ensure_web(&shared, &weak, &web_url, port, false, false);
        });
    }

    {
        let tx = tx.clone();
        let shared = shared.clone();
        ui.on_split_import_clicked(move || {
            if shared.busy.load(Ordering::SeqCst) {
                return;
            }
            let dir = {
                let cfg = shared.cfg.lock().unwrap();
                export_dir_of(&cfg)
            };
            if let Some(files) = rfd::FileDialog::new()
                .set_title("选择要按稿件拆解的 JSON")
                .set_directory(dir)
                .add_filter("JSON", &["json"])
                .pick_files()
            {
                let _ = tx.send(Cmd::SplitImport(files));
            }
        });
    }

    // ---- 导入 ----
    {
        let tx = tx.clone();
        let shared = shared.clone();
        let weak = weak.clone();
        ui.on_import_clicked(move || {
            let dir = shared.cfg.lock().unwrap().last_dir.clone();
            let mut dialog = rfd::FileDialog::new()
                .set_title("导入 JSON（可多选，自动合并去重）")
                .add_filter("JSON 文件", &["json"]);
            if !dir.is_empty() {
                dialog = dialog.set_directory(&dir);
            }
            if let Some(files) = dialog.pick_files() {
                if !files.is_empty() {
                    let _ = tx.send(Cmd::Import(files));
                }
            }
            let _ = &weak;
        });
    }

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

    // ---- 设置 ----
    {
        let shared = shared.clone();
        let weak = weak.clone();
        ui.on_settings_open(move || {
            let (port, dir, auto, delay, delay_max) = {
                let cfg = shared.cfg.lock().unwrap();
                (
                    cfg.web_port.to_string(),
                    export_dir_of(&cfg),
                    cfg.auto_export,
                    cfg.interval_ms.to_string(),
                    cfg.interval_max_ms.to_string(),
                )
            };
            emit(&weak, move |w| {
                w.set_web_port_text(port.into());
                w.set_export_dir_text(dir.into());
                w.set_auto_export(auto);
                w.set_page_delay_text(delay.into());
                w.set_page_delay_max_text(delay_max.into());
                w.set_settings_visible(true);
            });
        });
    }

    {
        let shared = shared.clone();
        let weak = weak.clone();
        ui.on_settings_apply(move || {
            let (port_txt, dir, auto, delay_txt, delay_max_txt) = match weak.upgrade() {
                Some(w) => (
                    w.get_web_port_text().to_string(),
                    w.get_export_dir_text().to_string(),
                    w.get_auto_export(),
                    w.get_page_delay_text().to_string(),
                    w.get_page_delay_max_text().to_string(),
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
            let mut cfg = shared.cfg.lock().unwrap();
            let mut next = cfg.clone();
            next.web_port = port;
            next.interval_ms = low;
            next.interval_max_ms = high;
            next.export_dir = dir;
            next.auto_export = auto;
            match next.save() {
                Ok(()) => {
                    *cfg = next;
                    set_foot(&weak, "设置已保存".into());
                }
                Err(e) => port_alert(&weak, format!("保存设置失败：{}", e)),
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
                let auto = cfg.auto_export;
                let delay = cfg.interval_ms.to_string();
                let delay_max = cfg.interval_max_ms.to_string();
                drop(cfg);
                w.set_settings_visible(false);
                w.set_web_port_text(port.into());
                w.set_export_dir_text(dir.into());
                w.set_auto_export(auto);
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
        let weak=weak.clone();
        ui.on_range_edited(move |ty| {if let Some(w)=weak.upgrade() {clamp_range_input(&w,ty as u32);}});
    }
    {
        let weak=weak.clone();let tx=tx.clone();let shared=shared.clone();
        ui.on_range_all(move |ty| {
            if shared.busy.load(Ordering::SeqCst) {return;}
            let Some(w)=weak.upgrade() else {return;};
            if select_all_range(&w,ty as u32) {
                if shared.account_uid.load(Ordering::SeqCst)>0 {
                    set_foot(&weak,format!("正在获取{}总页数…",checkpoint::label(ty as u32)));
                    let _=tx.send(Cmd::QueryTotals(vec![ty as u32]));
                } else {
                    set_foot(&weak,"已选择全部，请先登录以获取总页数".into());
                }
            }
        });
    }
    {
        let tx = tx.clone();
        let shared = shared.clone();
        ui.on_range_enabled(move |ty, enabled| {
            if enabled
                && shared.account_uid.load(Ordering::SeqCst) > 0
                && collection::TYPES.contains(&(ty as u32))
            {
                let _ = tx.send(Cmd::QueryTotals(vec![ty as u32]));
            }
        });
    }

    // ---- 清空 ----
    {
        let shared = shared.clone();
        let weak = weak.clone();
        ui.on_clear_all(move || {
            if shared.busy.load(Ordering::SeqCst) {
                return;
            }
            shared.rows.lock().unwrap().clear();
            *shared.doc.lock().unwrap() = ArchiveDoc::default();
            refresh_rows(&shared, &weak);
            refresh_stats(&shared, &weak);
            clear_checkpoint(&shared, &weak);
            push_log(&shared, &weak, "已清空当前列表与数据");
            set_foot(&weak, "已清空".into());
            set_status(&weak, "未连接".into(), false);
            emit(&weak, |w| w.set_account_text("".into()));
        });
    }

    // ---- 风控弹窗：后台继续等 ----
    {
        let weak = weak.clone();
        ui.on_verify_dismiss(move || {
            emit(&weak,|w|{w.set_verify_background(true);w.set_verify_visible(false);});
        });
    }

    // ---- 默认启动网页服务：没数据也先起，之后采集到的数据会直接出现在页面上 ----
    {
        let port = shared.cfg.lock().unwrap().web_port;
        ensure_web(&shared, &weak, &web_url, port, false, false);
    }

    ui.run()
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
            let restored = task.doc();
            {
                let mut doc = shared.doc.lock().unwrap();
                for video in restored.videos {
                    doc.merge_video(video);
                }
            }
            if task.mode == "selected" {
                let mut rows = shared.rows.lock().unwrap();
                for target in &task.targets {
                    if let Some((v, selected)) = rows
                        .iter_mut()
                        .find(|(v, _)| checkpoint::key(v) == checkpoint::key(target))
                    {
                        *v = target.clone();
                        *selected = true;
                    } else {
                        rows.push((target.clone(), true));
                    }
                }
            }
            let scope = if task.mode == "all" { 0 } else { 1 };
            emit(ui, move |w| w.set_scope(scope));
            push_log(shared, ui, format!("已恢复采集进度：{}", task.summary()));
            active_task(shared, ui, &task);
            refresh_rows(shared, ui);
            refresh_stats(shared, ui);
        }
        None => (),
    }
}

fn set_account(shared: &Arc<Shared>, ui: &Weak<AppWindow>, uid: u64) {
    let old = shared.account_uid.swap(uid, Ordering::SeqCst);
    if old != uid {
        for ty in collection::TYPES {
            set_type_total(ui, ty, None);
        }
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
        click(&ui, 870., 450.); // blank inside the card
        assert_eq!(closed.get(), 0, "card blank area must consume clicks");
        click(&ui, 455., 320.); // port input
        assert_eq!(closed.get(), 0, "editing a setting must not close the card");
        click(&ui, 817., 610.); // bottom-right Save button
        assert_eq!(
            saved.get(),
            1,
            "Save button must fit inside the compact card"
        );
        assert_eq!(closed.get(), 0, "Save must not close the card");
        assert!(ui.get_settings_visible());
        click(&ui, 847., 289.);
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
        export::export_item(
            root.path(),
            &Video {
                aid: 42,
                owner_mid: 123,
                comments: vec![model::Comment {
                    rpid: 7,
                    ..Default::default()
                }],
                ..Default::default()
            },
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
    fn all_button_and_upper_bound_correction_work_for_every_type() {
        let window=MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window))).unwrap();
        let ui=AppWindow::new().unwrap();
        ui.set_range_video_limit("7".into());
        ui.set_range_video_from("3".into());ui.set_range_video_to("999".into());
        clamp_range_input(&ui,1);assert_eq!(ui.get_range_video_to().as_str(),"7");
        assert!(!select_all_range(&ui,1));
        assert_eq!(ui.get_range_video_from().as_str(),"1");assert_eq!(ui.get_range_video_to().as_str(),"7");
        ui.set_range_dynamic_limit("7".into());
        ui.set_range_dynamic_from("3".into());ui.set_range_dynamic_to("999".into());
        clamp_range_input(&ui,11);assert_eq!(ui.get_range_dynamic_to().as_str(),"7");
        assert!(!select_all_range(&ui,11));
        assert_eq!(ui.get_range_dynamic_from().as_str(),"1");assert_eq!(ui.get_range_dynamic_to().as_str(),"7");
        ui.set_range_repost_limit("7".into());
        ui.set_range_repost_from("3".into());ui.set_range_repost_to("999".into());
        clamp_range_input(&ui,17);assert_eq!(ui.get_range_repost_to().as_str(),"7");
        assert!(!select_all_range(&ui,17));
        assert_eq!(ui.get_range_repost_from().as_str(),"1");assert_eq!(ui.get_range_repost_to().as_str(),"7");
        ui.set_range_article_limit("7".into());
        ui.set_range_article_from("3".into());ui.set_range_article_to("999".into());
        clamp_range_input(&ui,12);assert_eq!(ui.get_range_article_to().as_str(),"7");
        assert!(!select_all_range(&ui,12));
        assert_eq!(ui.get_range_article_from().as_str(),"1");assert_eq!(ui.get_range_article_to().as_str(),"7");
        ui.set_range_audio_limit("7".into());
        ui.set_range_audio_from("3".into());ui.set_range_audio_to("999".into());
        clamp_range_input(&ui,14);assert_eq!(ui.get_range_audio_to().as_str(),"7");
        assert!(!select_all_range(&ui,14));
        assert_eq!(ui.get_range_audio_from().as_str(),"1");assert_eq!(ui.get_range_audio_to().as_str(),"7");
        ui.set_range_audio_limit("".into());assert!(select_all_range(&ui,14));
        assert!(ui.get_range_audio_all_pending());
        ui.set_range_audio_limit("4".into());assert!(!select_all_range(&ui,14));
        assert_eq!(ui.get_range_audio_to().as_str(),"4");
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

fn load_history(shared: &Arc<Shared>, ui: &Weak<AppWindow>, uid: u64, expected_cookie: &str) {
    let root = {
        let cfg = shared.cfg.lock().unwrap();
        export_dir_of(&cfg)
    };
    let imported = export::load_user_exports(std::path::Path::new(&root), uid);
    // Hold the credential lock until the result is applied so a superseded login
    // cannot insert a different account's data during an account transition.
    let cfg = shared.cfg.lock().unwrap();
    if cfg.sessdata != expected_cookie || shared.account_uid.load(Ordering::SeqCst) != uid {
        return;
    }
    {
        let mut doc = shared.doc.lock().unwrap();
        for video in imported.doc.videos {
            doc.merge_video(video);
        }
    }
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
    let videos = shared.doc.lock().unwrap().videos.clone();
    let mut rows = shared.rows.lock().unwrap();
    for video in videos {
        if let Some((existing, _)) = rows
            .iter_mut()
            .find(|(v, _)| checkpoint::key(v) == checkpoint::key(&video))
        {
            *existing = video;
        } else {
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
            Ok((_, total)) => set_type_total(ui, ty, Some(total)),
            Err(e) => {
                set_type_total(ui, ty, None);
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
fn set_type_total(ui:&Weak<AppWindow>,ty:u32,total:Option<u64>) {
    let pages=total.map(|n|n.div_ceil(collection::PAGE_SIZE));
    let text=total.map(|n|format!("共 {} 页 / {} 条",n.div_ceil(collection::PAGE_SIZE),n)).unwrap_or_else(||"未获取".into());
    let limit=pages.map(|n|n.to_string()).unwrap_or_default();
    emit(ui,move |w| {
        match ty {
            1=>{
                let pending=w.get_range_video_all_pending();
                w.set_range_video_total(text.into());w.set_range_video_limit(limit.into());
                if pending && pages.is_some() {select_all_range(w,1);}
                w.set_range_video_all_pending(false);
            }
            11=>{
                let pending=w.get_range_dynamic_all_pending();
                w.set_range_dynamic_total(text.into());w.set_range_dynamic_limit(limit.into());
                if pending && pages.is_some() {select_all_range(w,11);}
                w.set_range_dynamic_all_pending(false);
            }
            17=>{
                let pending=w.get_range_repost_all_pending();
                w.set_range_repost_total(text.into());w.set_range_repost_limit(limit.into());
                if pending && pages.is_some() {select_all_range(w,17);}
                w.set_range_repost_all_pending(false);
            }
            12=>{
                let pending=w.get_range_article_all_pending();
                w.set_range_article_total(text.into());w.set_range_article_limit(limit.into());
                if pending && pages.is_some() {select_all_range(w,12);}
                w.set_range_article_all_pending(false);
            }
            14=>{
                let pending=w.get_range_audio_all_pending();
                w.set_range_audio_total(text.into());w.set_range_audio_limit(limit.into());
                if pending && pages.is_some() {select_all_range(w,14);}
                w.set_range_audio_all_pending(false);
            }
            _=>return,
        }
        clamp_range_input(w,ty);
    });
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
fn set_range_bounds(w:&AppWindow,ty:u32,from:String,to:String) {
    match ty {
        1=>{w.set_range_video_from(from.into());w.set_range_video_to(to.into());},
        11=>{w.set_range_dynamic_from(from.into());w.set_range_dynamic_to(to.into());},
        17=>{w.set_range_repost_from(from.into());w.set_range_repost_to(to.into());},
        12=>{w.set_range_article_from(from.into());w.set_range_article_to(to.into());},
        14=>{w.set_range_audio_from(from.into());w.set_range_audio_to(to.into());},
        _=>(),
    }
}
fn clamp_range_input(w:&AppWindow,ty:u32) {
    if let Some((from,to,limit))=range_input(w,ty) {
        let (from,to)=collection::corrected_bounds(&from,&to,limit);set_range_bounds(w,ty,from,to);
    }
}
fn select_all_range(w:&AppWindow,ty:u32)->bool {
    let Some((_,_,limit))=range_input(w,ty) else {return false;};
    set_range_bounds(w,ty,"1".into(),limit.filter(|n|*n>0).map(|n|n.to_string()).unwrap_or_default());
    let pending=limit.is_none();
    match ty {
        1=>w.set_range_video_all_pending(pending),
        11=>w.set_range_dynamic_all_pending(pending),
        17=>w.set_range_repost_all_pending(pending),
        12=>w.set_range_article_all_pending(pending),
        14=>w.set_range_audio_all_pending(pending),
        _=>(),
    }
    pending
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
