use crate::model::{ts_text, Comment, Reply, Video};
use anyhow::{anyhow, bail, Result};
use md5::{Digest, Md5};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub const UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
Chrome/131.0.0.0 Safari/537.36";

/// 设备指纹接口：返回 b_3 / b_4，即 buvid3 / buvid4。
/// 带上它以后 -352 触发率明显下降（B 站会按 buvid 做频控，缺指纹时更容易被判成脚本）。
pub const FINGER_SPI: &str = "https://api.bilibili.com/x/frontend/finger/spi";

/// WBI 签名用的字符混排表
const MIXIN_KEY_ENC_TAB: [usize; 64] = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42, 19, 29,
    28, 14, 39, 12, 38, 41, 13, 37, 48, 7, 16, 24, 55, 40, 61, 26, 17, 0, 1, 60, 51, 30, 4, 22, 25,
    54, 21, 56, 59, 6, 63, 57, 62, 11, 36, 20, 34, 44, 52,
];

/// 统一的 HTTP 客户端：浏览器 UA + HTTP/1.1（HTTP/2 下评论接口不返回 IP 属地）
/// 显式代理设置（空串 = 强制直连）。进程内只生效一次，启动时最先调用。
static PROXY: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub fn set_proxy(p: &str) {
    let _ = PROXY.set(p.trim().to_string());
}

fn proxy_url() -> String {
    if let Some(p) = PROXY.get() {
        return p.clone();
    }
    std::env::var("BILI_HUB_PROXY")
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// 统一的 HTTP 客户端。
///
/// 默认**强制直连**：很多机器的 `http_proxy` / `https_proxy` 环境变量指向一个
/// 已经不通（或只放行白名单）的本地端口，reqwest 默认会照单全收，于是所有请求
/// 都卡在 CONNECT 隧道上超时 —— 表现就是「二维码获取不到」「接口没反应」。
/// 要真走代理时用配置里的 `proxy` 字段或 `BILI_HUB_PROXY` 显式指定。
pub fn http_client() -> Result<reqwest::blocking::Client> {
    let mut b = reqwest::blocking::Client::builder()
        .user_agent(UA)
        .timeout(Duration::from_secs(30))
        .http1_only();
    let p = proxy_url();
    b = if p.is_empty() {
        b.no_proxy()
    } else {
        match reqwest::Proxy::all(&p) {
            Ok(pr) => b.proxy(pr),
            Err(_) => b.no_proxy(),
        }
    };
    Ok(b.build()?)
}

/// 取设备指纹。失败返回空串，不影响后续请求（只是更容易撞风控）
pub fn fetch_fingerprint(http: &reqwest::blocking::Client) -> (String, String) {
    let resp = http
        .get(FINGER_SPI)
        .header("Referer", "https://www.bilibili.com/")
        .header("Accept", "*/*")
        .send();
    let Ok(resp) = resp else {
        return (String::new(), String::new());
    };
    let Ok(v) = resp.json::<Value>() else {
        return (String::new(), String::new());
    };
    let b3 = v["data"]["b_3"].as_str().unwrap_or("").trim().to_string();
    let b4 = v["data"]["b_4"].as_str().unwrap_or("").trim().to_string();
    (b3, b4)
}

/// buvid 必须带 infoc 后缀；接口返回的已经带了，别重复加
fn with_infoc(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        return String::new();
    }
    if t.ends_with("infoc") {
        t.to_string()
    } else {
        format!("{}infoc", t)
    }
}

pub struct Bili {
    http: reqwest::blocking::Client,
    cookie: String,
    buvid3: String,
    buvid4: String,
    /// 「首次访问时间」，同一个 buvid 应当固定，别每次请求都变
    bnut: i64,
    wbi: Option<(String, String)>,
    /// 请求间隔范围（毫秒），每次翻页在区间内随机取一个值，避免固定节奏触发风控
    pub interval_min_ms: u64,
    pub interval_max_ms: u64,
}

/// 每翻一页等待多久：在 [min, max] 内随机取一个整数毫秒。
/// 间隔抖动不需要密码学随机，用「单调时间低位的纳秒 + 原子计数器」做种子即可，
/// 零依赖且比固定值更难被风控按节奏识别。min>=max 时退化为固定 min。
pub fn random_delay_ms(min: u64, max: u64) -> u64 {
    let (lo, hi) = if min <= max { (min, max) } else { (max, min) };
    if lo == hi {
        return lo;
    }
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static SEED: AtomicU64 = AtomicU64::new(0x9E37_79B9_7F4A_7C15);
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let mut x = SEED.fetch_add(1, Ordering::Relaxed) ^ t.rotate_left(17);
    // xorshift64*，够用且快
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    let r = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
    lo + (r % (hi - lo + 1))
}

impl Bili {
    pub fn new(cookie: String) -> Result<Self> {
        let http = http_client()?;
        let (buvid3, buvid4) = fetch_fingerprint(&http);
        Ok(Self {
            http,
            cookie: normalize_cookie(&cookie),
            buvid3,
            buvid4,
            bnut: chrono::Local::now().timestamp(),
            wbi: None,
            interval_min_ms: 1800,
            interval_max_ms: 2400,
        })
    }

    /// 本次翻页该等多久（区间内随机）
    pub fn next_pause(&self) -> Duration {
        Duration::from_millis(random_delay_ms(self.interval_min_ms, self.interval_max_ms))
    }

    pub fn set_cookie(&mut self, cookie: &str) {
        self.cookie = normalize_cookie(cookie);
        self.wbi = None;
    }

    /// 让外部（扫码登录）把拿到的指纹塞进来
    pub fn set_fingerprint(&mut self, buvid3: &str, buvid4: &str) {
        self.buvid3 = buvid3.trim().to_string();
        self.buvid4 = buvid4.trim().to_string();
    }

    pub fn fingerprint(&self) -> (&str, &str) {
        (&self.buvid3, &self.buvid4)
    }

    /// 真正的 Cookie 头：用户 Cookie + buvid3 / buvid4 + b_nut
    fn cookie_header(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        let lower = self.cookie.to_ascii_lowercase();
        if !self.cookie.is_empty() {
            parts.push(self.cookie.clone());
        }
        if !self.buvid3.is_empty() && !lower.contains("buvid3=") {
            parts.push(format!("buvid3={}", with_infoc(&self.buvid3)));
        }
        if !self.buvid4.is_empty() && !lower.contains("buvid4=") {
            parts.push(format!("buvid4={}", with_infoc(&self.buvid4)));
        }
        if !lower.contains("b_nut=") {
            parts.push(format!("b_nut={}", self.bnut));
        }
        parts.join("; ")
    }

    fn get(&self, url: &str, referer: &str) -> Result<Value> {
        let cookie = self.cookie_header();
        let mut req = self
            .http
            .get(url)
            .header("Referer", referer)
            .header("Accept", "application/json, text/plain, */*")
            .header("Accept-Language", "zh-CN,zh;q=0.9");
        if !cookie.is_empty() {
            req = req.header("Cookie", cookie);
        }
        let resp = req.send()?;
        let status = resp.status();
        let body = resp.text()?;
        if !status.is_success() {
            bail!("HTTP {}：{}", status.as_u16(), snippet(&body));
        }
        let v: Value = serde_json::from_str(&body)
            .map_err(|_| anyhow!("返回内容不是 JSON：{}", snippet(&body)))?;
        Ok(v)
    }

    fn get_api(&self, url: &str, referer: &str) -> Result<Value> {
        let v = self.get(url, referer)?;
        let code = v.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
        if code != 0 {
            let msg = v
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("未知错误");
            bail!("{}（code {}）", friendly(code, msg), code);
        }
        Ok(v)
    }

    /// 获取 WBI 签名所需的 img_key / sub_key
    pub fn refresh_wbi(&mut self) -> Result<()> {
        let v = self.get(
            "https://api.bilibili.com/x/web-interface/nav",
            "https://www.bilibili.com/",
        )?;
        let img = v["data"]["wbi_img"]["img_url"].as_str().unwrap_or("");
        let sub = v["data"]["wbi_img"]["sub_url"].as_str().unwrap_or("");
        if img.is_empty() || sub.is_empty() {
            bail!("无法获取 WBI 密钥，请检查网络");
        }
        self.wbi = Some((file_stem(img), file_stem(sub)));
        Ok(())
    }

    /// 判断 Cookie 是否处于登录状态
    pub fn login_state(&self) -> Result<(bool, String, u64)> {
        let v = self.get(
            "https://api.bilibili.com/x/web-interface/nav",
            "https://www.bilibili.com/",
        )?;
        let is_login = v["data"]["isLogin"].as_bool().unwrap_or(false);
        let uname = v["data"]["uname"].as_str().unwrap_or("").to_string();
        let mid = v["data"]["mid"].as_u64().unwrap_or(0);
        Ok((is_login, uname, mid))
    }

    fn sign(&self, params: &[(String, String)]) -> Result<String> {
        let (img, sub) = self.wbi.as_ref().ok_or_else(|| anyhow!("WBI 密钥未就绪"))?;
        let raw = format!("{}{}", img, sub);
        let bytes = raw.as_bytes();
        let mixin: String = MIXIN_KEY_ENC_TAB
            .iter()
            .take(32)
            .map(|&i| bytes.get(i).copied().unwrap_or(b'0') as char)
            .collect();

        let mut list: Vec<(String, String)> = params.to_vec();
        list.push((
            "wts".to_string(),
            chrono::Local::now().timestamp().to_string(),
        ));
        list.sort_by(|a, b| a.0.cmp(&b.0));

        let query = list
            .iter()
            .map(|(k, v)| {
                let cleaned: String = v.chars().filter(|c| !"!'()*".contains(*c)).collect();
                format!(
                    "{}={}",
                    urlencoding::encode(k),
                    urlencoding::encode(&cleaned)
                )
            })
            .collect::<Vec<_>>()
            .join("&");

        let mut hasher = Md5::new();
        hasher.update(format!("{}{}", query, mixin).as_bytes());
        let w_rid = format!("{:x}", hasher.finalize());
        Ok(format!("{}&w_rid={}", query, w_rid))
    }

    /// 创作中心稿件列表（需要 SESSDATA）
    pub fn archives_page(&self, pn: u64, ps: u64) -> Result<(Vec<Video>, u64)> {
        let url = format!(
            "https://member.bilibili.com/x/web/archives?status=pubed,is_pubing&pn={}&ps={}&coop=1&interactive=1",
            pn, ps
        );
        let v = self.get_api(
            &url,
            "https://member.bilibili.com/platform/inter-active/comment",
        )?;
        let data = &v["data"];
        let total = data["page"]["total"].as_u64().unwrap_or(0);
        let mut out = Vec::new();
        if let Some(list) = data["arc_audits"].as_array() {
            for item in list {
                let a = &item["Archive"];
                if a.is_null() {
                    continue;
                }
                let aid = num(&a["aid"]);
                let bvid = a["bvid"].as_str().unwrap_or("").to_string();
                let title = a["title"].as_str().unwrap_or("").to_string();
                let pubdate = a["ptime"]
                    .as_i64()
                    .or_else(|| a["pubdate"].as_i64())
                    .unwrap_or(0);
                out.push(Video {
                    status: "unknown".into(),
                    status_text: String::new(),
                    bvid: bvid.clone(),
                    aid,
                    title,
                    owner_mid: 0,
                    owner_name: String::new(),
                    url: Video::url_for(&bvid, aid),
                    cover: a["cover"].as_str().unwrap_or("").to_string(),
                    publish_time: pubdate,
                    publish_time_text: ts_text(pubdate),
                    comment_total: num(&a["stat"]["reply"]),
                    comments: Vec::new(),
                    ..Default::default()
                });
            }
        }
        Ok((out, total))
    }

    /// 通过 BV 号 / av 号 / 链接 / aid 获取稿件信息
    pub fn video_info(&self, input: &str) -> Result<Video> {
        let raw = input.trim();
        let (bvid, aid_opt) = parse_video_id(raw);
        if bvid.is_none() && aid_opt.is_none() {
            bail!("无法识别的视频标识：{}", raw);
        }
        let url = if let Some(b) = &bvid {
            format!("https://api.bilibili.com/x/web-interface/view?bvid={}", b)
        } else {
            format!(
                "https://api.bilibili.com/x/web-interface/view?aid={}",
                aid_opt.unwrap()
            )
        };
        let v = self.get_api(&url, "https://www.bilibili.com/")?;
        let d = &v["data"];
        let aid = num(&d["aid"]);
        let bvid_str = d["bvid"].as_str().unwrap_or("").to_string();
        let pubdate = d["pubdate"].as_i64().unwrap_or(0);
        Ok(Video {
            status: "unknown".into(),
            status_text: String::new(),
            bvid: bvid_str.clone(),
            aid,
            title: d["title"].as_str().unwrap_or("").to_string(),
            owner_mid: num(&d["owner"]["mid"]),
            owner_name: d["owner"]["name"].as_str().unwrap_or("").to_string(),
            url: Video::url_for(&bvid_str, aid),
            cover: d["pic"].as_str().unwrap_or("").to_string(),
            publish_time: pubdate,
            publish_time_text: ts_text(pubdate),
            comment_total: num(&d["stat"]["reply"]),
            comments: Vec::new(),
            ..Default::default()
        })
    }

    /// 抓取一个稿件的评论区（主评论 + 可选二级回复）
    /// 不再有页数上限：一直翻到接口说「没有了」为止
    pub fn comments_of(
        &mut self,
        video: &mut Video,
        include_replies: bool,
        cancel: &AtomicBool,
        mut on_page: impl FnMut(usize, u64),
    ) -> Result<()> {
        self.comments_of_resume(
            video,
            include_replies,
            cancel,
            &mut crate::checkpoint::Cursor::default(),
            |v, _| {
                on_page(v.comments.len(), v.comment_total);
                Ok(())
            },
        )
    }

    pub fn comments_of_resume(
        &mut self,
        video: &mut Video,
        include_replies: bool,
        cancel: &AtomicBool,
        progress: &mut crate::checkpoint::Cursor,
        mut on_page: impl FnMut(&Video, &crate::checkpoint::Cursor) -> Result<()>,
    ) -> Result<()> {
        if self.wbi.is_none() {
            self.refresh_wbi()?;
        }
        let referer = video.url.clone();
        let mut offset = progress.offset.clone();
        let mut seen: Vec<u64> = video.comments.iter().map(|c| c.rpid).collect();

        while !progress.main_done {
            if cancel.load(Ordering::SeqCst) {
                bail!("已取消");
            }
            let pagination = serde_json::json!({"offset":offset}).to_string();
            let params = vec![
                ("oid".to_string(), video.aid.to_string()),
                ("type".to_string(), video.reply_type().to_string()),
                ("mode".to_string(), "2".to_string()),
                ("pagination_str".to_string(), pagination),
                ("plat".to_string(), "1".to_string()),
                ("web_location".to_string(), "1315875".to_string()),
            ];
            let query = self.sign(&params)?;
            let url = format!("https://api.bilibili.com/x/v2/reply/wbi/main?{}", query);
            let v = self.get_api(&url, &referer)?;
            let data = &v["data"];

            if let Some(cursor) = data.get("cursor") {
                if let Some(t) = cursor["all_count"].as_u64() {
                    if t > 0 {
                        video.comment_total = t;
                    }
                }
            }

            // 置顶评论（UP 主置顶 / 管理员置顶）
            let mut pinned_rpids: Vec<u64> = Vec::new();
            if let Some(list) = data["top_replies"].as_array() {
                for r in list {
                    if let Some(c) = parse_comment(r, video.owner_mid) {
                        if !seen.contains(&c.rpid) {
                            seen.push(c.rpid);
                            pinned_rpids.push(c.rpid);
                            video.comments.push(Comment {
                                is_pinned: true,
                                ..c
                            });
                        }
                    }
                }
            }
            if data["upper"]["top"].is_object() {
                if let Some(c) = parse_comment(&data["upper"]["top"], video.owner_mid) {
                    if !seen.contains(&c.rpid) {
                        seen.push(c.rpid);
                        pinned_rpids.push(c.rpid);
                        video.comments.push(Comment {
                            is_pinned: true,
                            ..c
                        });
                    }
                }
            }

            if let Some(list) = data["replies"].as_array() {
                for r in list {
                    if let Some(c) = parse_comment(r, video.owner_mid) {
                        if seen.contains(&c.rpid) {
                            continue;
                        }
                        seen.push(c.rpid);
                        video.comments.push(c);
                    }
                }
            }

            let mut is_end = true;
            if let Some(cursor) = data.get("cursor") {
                is_end = cursor["is_end"].as_bool().unwrap_or(true);
                let next = cursor["pagination_reply"]["next_offset"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                if next.is_empty() {
                    is_end = true;
                } else {
                    offset = next;
                }
            }
            if !is_end && offset == progress.offset {
                bail!("评论游标未推进，已保留进度，请稍后重试");
            }
            progress.offset = offset.clone();
            progress.main_done = is_end;
            on_page(video, progress)?;
            if is_end {
                break;
            }
            std::thread::sleep(self.next_pause());
        }

        // 二级回复（不设页数上限，抓到该楼层没有更多为止）
        if include_replies {
            let targets: Vec<(usize, u64)> = video
                .comments
                .iter()
                .enumerate()
                .filter(|(_, c)| c.reply_count > 0)
                .map(|(i, c)| (i, c.rpid))
                .collect();
            for (idx, rpid) in targets {
                if cancel.load(Ordering::SeqCst) {
                    bail!("已取消");
                }
                let mut pn = *progress.replies.get(&rpid).unwrap_or(&1);
                if pn == 0 {
                    continue;
                }
                loop {
                    if cancel.load(Ordering::SeqCst) {
                        bail!("已取消");
                    }
                    let url = format!(
                        "https://api.bilibili.com/x/v2/reply/reply?oid={}&type={}&root={}&ps=20&pn={}",
                        video.aid, video.reply_type(), rpid, pn
                    );
                    let v = self.get_api(&url, &referer)?;
                    let list = match v["data"]["replies"].as_array() {
                        Some(l) if !l.is_empty() => l.clone(),
                        _ => {
                            progress.replies.insert(rpid, 0);
                            on_page(video, progress)?;
                            break;
                        }
                    };
                    if let Some(c) = video.comments.get_mut(idx) {
                        for r in &list {
                            if let Some(rep) = parse_reply(r, video.owner_mid) {
                                if !c.replies.iter().any(|x| x.rpid == rep.rpid) {
                                    c.replies.push(rep);
                                }
                            }
                        }
                        c.replies.sort_by_key(|r| r.ctime);
                    }
                    if list.len() < 20 {
                        progress.replies.insert(rpid, 0);
                        on_page(video, progress)?;
                        break;
                    }
                    pn += 1;
                    progress.replies.insert(rpid, pn);
                    on_page(video, progress)?;
                    std::thread::sleep(self.next_pause());
                }
            }
        }

        video.comments.sort_by(|a, b| {
            b.is_pinned
                .cmp(&a.is_pinned)
                .then_with(|| b.ctime.cmp(&a.ctime))
        });
        Ok(())
    }

    /// 补全 IP 属地。
    /// 创作中心的评论管理接口不返回 IP，只有公开侧评论接口（wbi/main）带
    /// `reply_control.location`，所以只对「公开」稿件尝试；已删除 / 仅自己可见的
    /// 稿件评论功能已关闭（12002），拿不到 IP，留空。
    pub fn fill_locations(
        &mut self,
        videos: &mut [Video],
        cancel: &AtomicBool,
        mut on_tick: impl FnMut(usize, usize, usize),
    ) -> (usize, usize) {
        // 创作中心接口（fulllist）不返回 IP 字段，只有公开评论接口的
        // reply_control.location 才带；所以这里只处理「公开且确实缺 IP」的稿件，
        // 已经全员有 IP 的直接跳过，不再白发请求。
        let mut targets: Vec<usize> = Vec::new();
        for (i, v) in videos.iter().enumerate() {
            if v.status != "public" || v.comments.is_empty() {
                continue;
            }
            let missing = v.comments.iter().any(|c| c.location.is_empty())
                || v.comments
                    .iter()
                    .any(|c| c.replies.iter().any(|r| r.location.is_empty()));
            if missing {
                targets.push(i);
            }
        }
        let total = targets.len();
        let mut filled = 0usize;
        let mut all = 0usize;
        if total == 0 {
            // 公开稿件全都已有 IP，一次请求都不用发
            return (0, 0);
        }
        if self.wbi.is_none() && self.refresh_wbi().is_err() {
            return (0, 0);
        }
        for (k, vi) in targets.into_iter().enumerate() {
            if cancel.load(Ordering::SeqCst) {
                break;
            }
            on_tick(k, total, filled);
            let video = &mut videos[vi];
            all += video.comments.len() + video.total_replies();
            let mut map: HashMap<u64, String> = HashMap::new();

            // 主评论分页：只抓够覆盖已抓到的评论数
            let referer = if video.url.is_empty() {
                "https://www.bilibili.com/"
            } else {
                video.url.as_str()
            };
            let max_pages = (video.comments.len() + 19) / 20 + 1;
            let mut offset = String::new();
            let mut page = 0usize;
            while page < max_pages && !cancel.load(Ordering::SeqCst) {
                page += 1;
                let pagination = format!("{{\"offset\":\"{}\"}}", offset);
                let params = vec![
                    ("oid".to_string(), video.aid.to_string()),
                    ("type".to_string(), video.comment_type.max(1).to_string()),
                    ("mode".to_string(), "2".to_string()),
                    ("pagination_str".to_string(), pagination),
                    ("plat".to_string(), "1".to_string()),
                    ("web_location".to_string(), "1315875".to_string()),
                ];
                let query = match self.sign(&params) {
                    Ok(q) => q,
                    Err(_) => break,
                };
                let url = format!("https://api.bilibili.com/x/v2/reply/wbi/main?{}", query);
                let v = match self.get_api(&url, referer) {
                    Ok(v) => v,
                    Err(_) => break,
                };
                let data = &v["data"];
                collect_locations(&data["top_replies"], &mut map);
                collect_locations(&data["replies"], &mut map);
                let is_end = data["cursor"]["is_end"].as_bool().unwrap_or(true);
                let next = data["cursor"]["pagination_reply"]["next_offset"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                if is_end || next.is_empty() {
                    break;
                }
                offset = next;
                std::thread::sleep(self.next_pause());
            }

            // 楼中楼：按楼层逐页补
            let roots: Vec<(u64, u64)> = video
                .comments
                .iter()
                .filter(|c| c.reply_count > 0)
                .map(|c| (c.rpid, c.reply_count))
                .collect();
            for (rpid, count) in roots {
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                let pages = ((count as usize + 19) / 20).max(1);
                for pn in 1..=pages {
                    let url = format!(
                        "https://api.bilibili.com/x/v2/reply/reply?oid={}&type={}&root={}&ps=20&pn={}",
                        video.aid,
                        video.comment_type.max(1),
                        rpid,
                        pn
                    );
                    let v = match self.get_api(&url, referer) {
                        Ok(v) => v,
                        Err(_) => break,
                    };
                    collect_locations(&v["data"]["replies"], &mut map);
                    std::thread::sleep(self.next_pause());
                }
            }

            for c in video.comments.iter_mut() {
                if c.location.is_empty() {
                    if let Some(s) = map.get(&c.rpid) {
                        c.location = s.clone();
                    }
                }
                for r in c.replies.iter_mut() {
                    if r.location.is_empty() {
                        if let Some(s) = map.get(&r.rpid) {
                            r.location = s.clone();
                        }
                    }
                }
            }
            filled += video
                .comments
                .iter()
                .filter(|c| !c.location.is_empty())
                .count()
                + video
                    .comments
                    .iter()
                    .flat_map(|c| c.replies.iter())
                    .filter(|r| !r.location.is_empty())
                    .count();
        }
        (filled, all)
    }
}

/// 从公开评论接口的回复数组里收集 rpid -> IP 属地（含楼中楼预览）
fn collect_locations(v: &Value, map: &mut HashMap<u64, String>) {
    if let Some(list) = v.as_array() {
        for r in list {
            let rpid = num(&r["rpid"]);
            let loc = location_of(r);
            if rpid != 0 && !loc.is_empty() {
                map.insert(rpid, loc);
            }
            collect_locations(&r["replies"], map);
        }
    }
}

fn normalize_cookie(raw: &str) -> String {
    let t = raw.trim().trim_matches('"').trim();
    if t.is_empty() {
        return String::new();
    }
    let joined = t
        .replace(['\r', '\n'], ";")
        .split(';')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("; ");

    let lower = joined.to_ascii_lowercase();
    if lower.contains("sessdata=") {
        return joined;
    }
    // 用户通常只复制 SESSDATA 的值：补上键名
    if joined.contains('%') || (joined.len() > 16 && !joined.contains('=')) {
        return format!("SESSDATA={}", joined);
    }
    joined
}

fn file_stem(url: &str) -> String {
    url.rsplit('/')
        .next()
        .unwrap_or("")
        .split('.')
        .next()
        .unwrap_or("")
        .to_string()
}

fn snippet(body: &str) -> String {
    let s: String = body.chars().take(160).collect();
    s.replace('\n', " ")
}

fn friendly(code: i64, msg: &str) -> String {
    match code {
        -101 => "账号未登录：请填写有效的 SESSDATA".to_string(),
        -403 => "访问被拒绝（-403）：Cookie 可能已过期或触发风控".to_string(),
        -412 => "请求过于频繁（-412）：请稍后重试或调大请求间隔".to_string(),
        -352 => "触发风控（-352）：请求过快，请等 3~5 分钟后重试".to_string(),
        -404 => "内容不存在或已删除".to_string(),
        62002 => "稿件不可见".to_string(),
        12002 => "评论区已关闭".to_string(),
        _ => msg.to_string(),
    }
}

fn num(v: &Value) -> u64 {
    v.as_u64()
        .or_else(|| v.as_i64().map(|i| i.max(0) as u64))
        .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
        .unwrap_or(0)
}

/// 评论者头像：新接口叫 avatar，老接口叫 face，统一成 https
fn avatar_url(member: &Value) -> String {
    let raw = member["avatar"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| member["face"].as_str())
        .unwrap_or("");
    https_cover(raw)
}

/// IP 属地：公开评论接口放在 reply_control.location，值形如「IP属地：上海」
fn location_of(v: &Value) -> String {
    let raw = v["reply_control"]["location"]
        .as_str()
        .or_else(|| v["reply_control"]["ip_location"].as_str())
        .unwrap_or("")
        .trim();
    if raw.is_empty() {
        return String::new();
    }
    raw.trim_start_matches("IP属地")
        .trim_start_matches(['：', ':', ' '])
        .trim()
        .to_string()
}

/// 图片评论：content.pictures[].img_src
fn pictures_of(v: &Value) -> Vec<String> {
    v["content"]["pictures"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|p| {
                    let src = p["img_src"]
                        .as_str()
                        .or_else(|| p["img_url"].as_str())
                        .unwrap_or("");
                    if src.trim().is_empty() {
                        None
                    } else {
                        Some(https_cover(src))
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 大会员：只看 vipStatus / accessStatus。
///
/// 注意 vipType 只是「档位」（1 月度 / 2 年度），过期用户的 vipType 依然是 1，
/// 拿它判断会把大批非会员也标成粉色昵称，所以不能当依据。
/// 少数接口不带 vipStatus，退回看 label.text（"大会员"/"年度大会员"，非会员为空）。
fn is_vip_of(member: &Value) -> bool {
    let vip = &member["vip"];
    if vip.is_null() {
        return false;
    }
    let access = vip["accessStatus"].as_i64().unwrap_or(0);
    match vip["vipStatus"].as_i64() {
        Some(status) => status == 1 || access == 1,
        None => {
            let label = vip["label"]["text"].as_str().unwrap_or("").trim();
            !label.is_empty() && vip["vipType"].as_i64().unwrap_or(0) > 0
        }
    }
}

fn parse_video_id(input: &str) -> (Option<String>, Option<u64>) {
    let t = input.trim();
    let lower = t.to_ascii_lowercase();
    // 纯数字视为 aid
    if t.chars().all(|c| c.is_ascii_digit()) && !t.is_empty() {
        return (None, t.parse::<u64>().ok());
    }
    // BV 号
    if let Some(pos) = lower.find("bv") {
        let rest = &t[pos..];
        let candidate: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        if candidate.len() == 12 {
            return (Some(candidate), None);
        }
    }
    // av 号
    if let Some(pos) = lower.find("/av") {
        let digits: String = t[pos + 3..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let Ok(id) = digits.parse::<u64>() {
            return (None, Some(id));
        }
    }
    if lower.starts_with("av") {
        let digits: String = t[2..].chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(id) = digits.parse::<u64>() {
            return (None, Some(id));
        }
    }
    (None, None)
}

fn parse_comment(v: &Value, up_mid: u64) -> Option<Comment> {
    if v.is_null() || !v.is_object() {
        return None;
    }
    let rpid = num(&v["rpid"]);
    if rpid == 0 {
        return None;
    }
    let mid = num(&v["member"]["mid"]);
    let ctime = v["ctime"].as_i64().unwrap_or(0);
    let content = v["content"]["message"].as_str().unwrap_or("").to_string();
    let pictures = pictures_of(v);
    if content.is_empty() && pictures.is_empty() {
        return None;
    }
    Some(Comment {
        rpid,
        mid,
        uname: v["member"]["uname"].as_str().unwrap_or("").to_string(),
        avatar: avatar_url(&v["member"]),
        level: num(&v["member"]["level_info"]["current_level"]) as u32,
        is_vip: is_vip_of(&v["member"]),
        location: location_of(v),
        pictures,
        is_up: up_mid != 0 && mid == up_mid,
        is_pinned: false,
        content,
        like: num(&v["like"]),
        ctime,
        ctime_text: ts_text(ctime),
        reply_count: num(&v["rcount"]).max(num(&v["count"])),
        replies: v["replies"]
            .as_array()
            .map(|list| list.iter().filter_map(|r| parse_reply(r, up_mid)).collect())
            .unwrap_or_default(),
    })
}

fn parse_reply(v: &Value, up_mid: u64) -> Option<Reply> {
    let rpid = num(&v["rpid"]);
    if rpid == 0 {
        return None;
    }
    let mid = num(&v["member"]["mid"]);
    let ctime = v["ctime"].as_i64().unwrap_or(0);
    let content = v["content"]["message"].as_str().unwrap_or("").to_string();
    let pictures = pictures_of(v);
    if content.is_empty() && pictures.is_empty() {
        return None;
    }
    Some(Reply {
        rpid,
        mid,
        uname: v["member"]["uname"].as_str().unwrap_or("").to_string(),
        avatar: avatar_url(&v["member"]),
        level: num(&v["member"]["level_info"]["current_level"]) as u32,
        is_vip: is_vip_of(&v["member"]),
        location: location_of(v),
        pictures,
        is_up: up_mid != 0 && mid == up_mid,
        content,
        like: num(&v["like"]),
        ctime,
        ctime_text: ts_text(ctime),
        parent: num(&v["parent"]),
    })
}

// ============================================================
// 创作中心「评论管理」：一次拿到全部稿件的评论
// GET /x/v2/reply/up/fulllist?type=1&oid=0&pn=&ps=
// oid=0 表示所有稿件（含已删除、仅自己可见的稿件）
// ============================================================

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CommentRow {
    pub rpid: u64,
    /// 评论所属内容类型：1 视频 / 11 动态 / 17 转发动态 / 12 专栏
    pub ty: u32,
    pub oid: u64,
    pub root: u64,
    pub parent: u64,
    pub mid: u64,
    pub uname: String,
    pub avatar: String,
    pub level: u32,
    pub is_vip: bool,
    pub location: String,
    pub pictures: Vec<String>,
    pub content: String,
    pub like: u64,
    pub ctime: i64,
    pub rcount: u64,
    pub bvid: String,
    pub title: String,
    pub cover: String,
    pub invisible: bool,
}

impl Bili {
    /// 拉取创作中心评论管理的一页。oid 传 0 表示该类型下的全部内容。
    /// ty：1 视频 / 11 动态 / 17 转发动态 / 12 专栏
    pub fn up_list_page(
        &self,
        ty: u32,
        pn: u64,
        ps: u64,
        oid: u64,
    ) -> Result<(Vec<CommentRow>, u64)> {
        // 参数与创作中心页面保持一致：order=1 按时间倒序
        let url = format!(
            "https://api.bilibili.com/x/v2/reply/up/fulllist?order=1&type={}&oid={}&pn={}&ps={}&charge_plus_filter=false",
            ty, oid, pn, ps
        );
        let v = self.get_api(
            &url,
            "https://member.bilibili.com/platform/inter-active/comment/article",
        )?;
        let total = v["data"]["page"]["total"].as_u64().unwrap_or(0);
        let empty = Vec::new();
        let list = v["data"]["list"].as_array().unwrap_or(&empty);
        // 我们以 type=ty 分类请求，每行的内容类型就是 ty 本身。
        // 返回体里的 `type` 是评论对象类型（常常恒为 1），不能直接拿来当内容类型，
        // 否则动态/专栏/音频评论会被误判成视频。统一用请求时的 ty 覆盖。
        Ok((
            list.iter().filter_map(|v| parse_row(v, ty)).collect(),
            total,
        ))
    }

    /// 带退避重试的翻页：-352 / -412 / -509 视为临时风控
    /// 连续抓到约 2500 条后接口会进入几分钟冷却，这里按 15s→…→300s 逐级等待，
    /// 一轮最长约 20 分钟。等待期间每秒回调 on_tick(剩余秒数)，界面可以显示倒计时，
    /// 用户也能去浏览器里过一下验证再回来。
    pub fn up_list_page_retry(
        &self,
        ty: u32,
        pn: u64,
        ps: u64,
        oid: u64,
        cancel: &AtomicBool,
        on_wait: &mut dyn FnMut(u64),
        on_tick: &mut dyn FnMut(u64),
    ) -> Result<(Vec<CommentRow>, u64)> {
        retry_with_risk_wait(
            || self.up_list_page(ty, pn, ps, oid), cancel, on_wait, on_tick,
            &mut |duration| std::thread::sleep(duration),
        )
    }

    /// 主页导航信息（昵称 / mid），用于标记 UP 主自己的回复
    pub fn me(&self) -> (u64, String) {
        match self.get(
            "https://api.bilibili.com/x/web-interface/nav",
            "https://www.bilibili.com/",
        ) {
            Ok(v) => (
                v["data"]["mid"].as_u64().unwrap_or(0),
                v["data"]["uname"].as_str().unwrap_or("").to_string(),
            ),
            Err(_) => (0, String::new()),
        }
    }
}

fn parse_row(v: &Value, ty: u32) -> Option<CommentRow> {
    let rpid = num(&v["rpid"]);
    if rpid == 0 {
        return None;
    }
    let content = v["content"]["message"].as_str().unwrap_or("").to_string();
    let pictures = pictures_of(v);
    if content.trim().is_empty() && pictures.is_empty() {
        return None;
    }
    Some(CommentRow {
        rpid,
        ty,
        oid: num(&v["oid"]),
        root: num(&v["root"]),
        parent: num(&v["parent"]),
        mid: num(&v["member"]["mid"]),
        uname: v["member"]["uname"].as_str().unwrap_or("").to_string(),
        avatar: avatar_url(&v["member"]),
        level: num(&v["member"]["level_info"]["current_level"]) as u32,
        is_vip: is_vip_of(&v["member"]),
        location: location_of(v),
        pictures,
        content,
        like: num(&v["like"]),
        ctime: v["ctime"].as_i64().unwrap_or(0),
        rcount: num(&v["rcount"]),
        bvid: v["bvid"].as_str().unwrap_or("").to_string(),
        title: v["title"].as_str().unwrap_or("").to_string(),
        cover: https_cover(v["cover_url"].as_str().unwrap_or("")),
        invisible: v["invisible"].as_bool().unwrap_or(false),
    })
}

/// 封面统一成 https，否则嵌到网页里会被浏览器当成混合内容拦掉
pub fn https_cover(url: &str) -> String {
    let u = url.trim();
    if let Some(rest) = u.strip_prefix("http://") {
        format!("https://{}", rest)
    } else {
        u.to_string()
    }
}

fn row_to_comment(row: &CommentRow, self_mid: u64) -> Comment {
    Comment {
        rpid: row.rpid,
        mid: row.mid,
        uname: row.uname.clone(),
        avatar: row.avatar.clone(),
        level: row.level,
        is_vip: row.is_vip,
        location: row.location.clone(),
        pictures: row.pictures.clone(),
        is_up: self_mid != 0 && row.mid == self_mid,
        is_pinned: false,
        content: row.content.clone(),
        like: row.like,
        ctime: row.ctime,
        ctime_text: ts_text(row.ctime),
        reply_count: row.rcount,
        replies: Vec::new(),
    }
}

fn row_to_reply(row: &CommentRow, self_mid: u64) -> Reply {
    Reply {
        rpid: row.rpid,
        mid: row.mid,
        uname: row.uname.clone(),
        avatar: row.avatar.clone(),
        level: row.level,
        is_vip: row.is_vip,
        location: row.location.clone(),
        pictures: row.pictures.clone(),
        is_up: self_mid != 0 && row.mid == self_mid,
        content: row.content.clone(),
        like: row.like,
        ctime: row.ctime,
        ctime_text: ts_text(row.ctime),
        parent: if row.parent == 0 {
            row.root
        } else {
            row.parent
        },
    }
}

// ============================================================
// 动态：原文 / IP 属地 / 赞评转 / 配图
// GET /x/polymer/web-dynamic/v1/feed/space?host_mid=&offset=&features=itemOpusStyle
// basic.comment_type 与评论管理的 type 是对得上的（11 图文、17 转发、1 视频投稿动态），
// basic.comment_id_str 就是评论行里的 oid，靠这两个字段把动态和评论接起来。
// ============================================================

#[derive(Debug, Clone, Default)]
pub struct DynamicInfo {
    /// 与评论管理的 type 一致：11 图文 / 17 转发
    pub comment_type: u32,
    /// 与评论行里的 oid 一致
    pub comment_id: u64,
    pub dynamic_id: u64,
    pub kind: String,
    pub title: String,
    pub text: String,
    pub pics: Vec<String>,
    pub ip_location: String,
    pub pub_ts: i64,
    pub like: u64,
    pub comment: u64,
    pub forward: u64,
    pub url: String,
    pub orig_text: String,
    pub orig_author: String,
    /// 正文被 summary 截断了，需要再取一次详情
    pub truncated: bool,
}

impl Bili {
    /// 动态详情里的完整正文（只有被截断的长文才需要单独请求）
    pub fn dynamic_detail_text(&self, dynamic_id: u64) -> Option<String> {
        let url = format!(
            "https://api.bilibili.com/x/polymer/web-dynamic/v1/detail?timezone_offset=-480&id={}",
            dynamic_id
        );
        let v = self.get_api(&url, "https://space.bilibili.com/").ok()?;
        let md = &v["data"]["item"]["modules"]["module_dynamic"];
        let a = vstr(&md["major"]["opus"]["summary"]["text"]);
        if a.is_empty() {
            Some(vstr(&md["desc"]["text"]))
        } else {
            Some(a)
        }
    }

    /// 翻完自己的动态列表。on_page 收到 (已翻页数, 累计条数)
    pub fn dynamic_feed(
        &self,
        mid: u64,
        cancel: &AtomicBool,
        mut on_page: impl FnMut(usize, usize),
    ) -> Result<Vec<DynamicInfo>> {
        let mut out: Vec<DynamicInfo> = Vec::new();
        let mut offset = String::new();
        let mut page = 0usize;
        let mut backoff = 0usize;
        loop {
            if cancel.load(Ordering::SeqCst) {
                break;
            }
            page += 1;
            let url = format!(
                "https://api.bilibili.com/x/polymer/web-dynamic/v1/feed/space?host_mid={}&offset={}&timezone_offset=-480&features=itemOpusStyle",
                mid,
                urlencoding::encode(&offset)
            );
            let v = match self.get_api(&url, "https://space.bilibili.com/") {
                Ok(v) => v,
                Err(e) => {
                    // 风控就退避重试，退几次还不行才放弃
                    if backoff < 3 {
                        backoff += 1;
                        std::thread::sleep(Duration::from_millis(4000 * backoff as u64));
                        page -= 1;
                        continue;
                    }
                    return Err(e);
                }
            };
            backoff = 0;
            let data = &v["data"];
            let empty = Vec::new();
            let items = data["items"].as_array().unwrap_or(&empty);
            for item in items {
                if let Some(mut info) = parse_dynamic(item) {
                    if info.truncated && info.dynamic_id > 0 {
                        if let Some(full) = self.dynamic_detail_text(info.dynamic_id) {
                            if full.chars().count() > info.text.chars().count() {
                                info.text = full;
                            }
                        }
                    }
                    out.push(info);
                }
            }
            on_page(page, out.len());
            let more = data["has_more"].as_bool().unwrap_or(false);
            if !more || items.is_empty() {
                break;
            }
            offset = data["offset"].as_str().unwrap_or("").to_string();
            std::thread::sleep(self.next_pause());
        }
        Ok(out)
    }
}

fn vstr(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

fn parse_dynamic(item: &Value) -> Option<DynamicInfo> {
    let basic = &item["basic"];
    let comment_type = basic["comment_type"].as_u64().unwrap_or(0) as u32;
    let comment_id = basic["comment_id_str"]
        .as_str()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    if comment_id == 0 || comment_type == 0 {
        return None;
    }
    let modules = &item["modules"];
    let md = &modules["module_dynamic"];
    let opus = &md["major"]["opus"];
    // 新式动态正文在 opus.summary，老式在 desc.text
    let text = {
        let a = vstr(&opus["summary"]["text"]);
        if a.is_empty() {
            vstr(&md["desc"]["text"])
        } else {
            a
        }
    };
    // 极少数长文会被截断（summary.has_more），调用方会再取一次详情补全文
    let truncated = opus["summary"]["has_more"].as_bool().unwrap_or(false);
    let mut pics: Vec<String> = Vec::new();
    if let Some(arr) = opus["pics"].as_array() {
        for p in arr {
            let u = vstr(&p["url"]);
            if !u.is_empty() {
                pics.push(https_cover(&u));
            }
        }
    }
    if pics.is_empty() {
        if let Some(arr) = md["major"]["draw"]["items"].as_array() {
            for p in arr {
                let u = vstr(&p["src"]);
                if !u.is_empty() {
                    pics.push(https_cover(&u));
                }
            }
        }
    }
    if pics.is_empty() {
        let cover = vstr(&md["major"]["archive"]["cover"]);
        if !cover.is_empty() {
            pics.push(https_cover(&cover));
        }
    }
    let au = &modules["module_author"];
    let pub_ts = au["pub_ts"]
        .as_str()
        .and_then(|s| s.parse::<i64>().ok())
        .or_else(|| au["pub_ts"].as_i64())
        .unwrap_or(0);
    let stat = &modules["module_stat"];
    let jump = vstr(&basic["jump_url"]);
    let url = if let Some(rest) = jump.strip_prefix("//") {
        format!("https://{}", rest)
    } else {
        jump
    };
    // 转发动态：把被转的那条也留下来，否则只剩「转发」两个字
    let (mut orig_text, mut orig_author) = (String::new(), String::new());
    if let Some(orig) = item.get("orig").filter(|o| !o.is_null()) {
        let omd = &orig["modules"]["module_dynamic"];
        orig_text = {
            let a = vstr(&omd["major"]["opus"]["summary"]["text"]);
            if a.is_empty() {
                vstr(&omd["desc"]["text"])
            } else {
                a
            }
        };
        orig_author = vstr(&orig["modules"]["module_author"]["name"]);
    }
    Some(DynamicInfo {
        comment_type,
        comment_id,
        dynamic_id: vstr(&item["id_str"]).parse().unwrap_or(0),
        kind: vstr(&item["type"]),
        title: vstr(&opus["title"]),
        text,
        pics,
        ip_location: vstr(&au["pub_location_text"]),
        pub_ts,
        like: num(&stat["like"]["count"]),
        comment: num(&stat["comment"]["count"]),
        forward: num(&stat["forward"]["count"]),
        url,
        orig_text,
        orig_author,
        truncated,
    })
}

/// 把扁平的行按稿件分组，并把楼中楼挂回父评论
pub fn rows_to_videos(
    rows: &[CommentRow],
    include_replies: bool,
    self_mid: u64,
    self_name: &str,
) -> Vec<Video> {
    let mut videos: Vec<Video> = Vec::new();
    // 不同内容类型下 oid 可能撞号，所以用 (类型, oid) 做键
    let mut index_of: HashMap<(u32, u64), usize> = HashMap::new();
    let mut root_index: HashMap<u64, (usize, usize)> = HashMap::new(); // rpid -> (video, comment)

    for row in rows.iter().filter(|r| r.root == 0) {
        let vi = *index_of.entry((row.ty, row.oid)).or_insert_with(|| {
            videos.push(new_video(row, self_mid, self_name));
            videos.len() - 1
        });
        let video = &mut videos[vi];
        root_index.insert(row.rpid, (vi, video.comments.len()));
        video.comments.push(row_to_comment(row, self_mid));
    }

    if include_replies {
        for row in rows.iter().filter(|r| r.root != 0) {
            match root_index.get(&row.root).copied() {
                Some((vi, ci)) => {
                    videos[vi].comments[ci]
                        .replies
                        .push(row_to_reply(row, self_mid));
                }
                None => {
                    // 父评论不在本次页范围时，保留为独立评论，避免丢数据
                    let vi = *index_of.entry((row.ty, row.oid)).or_insert_with(|| {
                        videos.push(new_video(row, self_mid, self_name));
                        videos.len() - 1
                    });
                    videos[vi].comments.push(row_to_comment(row, self_mid));
                }
            }
        }
    }

    for video in videos.iter_mut() {
        video.comment_total = video.comments.len() as u64;
        for c in video.comments.iter_mut() {
            if c.replies.len() as u64 > c.reply_count {
                c.reply_count = c.replies.len() as u64;
            }
            c.replies.sort_by_key(|r| r.ctime);
        }
        video.comments.sort_by(|a, b| b.ctime.cmp(&a.ctime));
    }
    videos.sort_by(|a, b| b.comments.len().cmp(&a.comments.len()));
    videos
}

fn new_video(row: &CommentRow, self_mid: u64, self_name: &str) -> Video {
    // type 11 / 17 是动态（图文 / 转发），12 专栏，14 音频，其余按视频处理
    let kind = match row.ty {
        11 | 17 => "dynamic",
        12 => "article",
        14 => "audio",
        _ => "video",
    };
    let url = match kind {
        // 真正的 opus 地址要拿到 dynamic_id 才能拼，后面补动态信息时再填
        "dynamic" => String::new(),
        "article" => format!("https://www.bilibili.com/read/cv{}", row.oid),
        "audio" => format!("https://www.bilibili.com/audio/au{}", row.oid),
        _ => Video::url_for(&row.bvid, row.oid),
    };
    Video {
        bvid: row.bvid.clone(),
        aid: row.oid,
        title: row.title.clone(),
        owner_mid: self_mid,
        owner_name: self_name.to_string(),
        url,
        cover: row.cover.clone(),
        kind: kind.to_string(),
        comment_type: row.ty.max(1),
        publish_time: 0,
        publish_time_text: String::new(),
        comment_total: 0,
        status: "unknown".to_string(),
        status_text: String::new(),
        comments: Vec::new(),
        ..Default::default()
    }
}

// ============================================================
// 稿件信息整合：评论行只带 title / bvid / cover，
// 时间、官方评论数、以及「是否已经不在稿件列表里」都要另外拿。
// GET member.bilibili.com/x/web/archives?status=all
// status=all 含已通过 + 未通过 + 仅自己可见，但不含已删除，
// 所以「存在评论、却不在这个列表里」的就是被删掉的稿件。
// ============================================================

#[derive(Debug, Clone, Default)]
pub struct ArchiveMeta {
    pub aid: u64,
    pub bvid: String,
    pub title: String,
    pub cover: String,
    pub ptime: i64,
    pub state: i64,
    pub state_desc: String,
    pub only_self: bool,
    pub reply: u64,
    pub view: u64,
}

impl ArchiveMeta {
    pub fn status(&self) -> (&'static str, String) {
        if self.only_self || self.state == -50 {
            ("private", "仅自己可见".to_string())
        } else if self.state == 0 {
            ("public", "公开".to_string())
        } else if !self.state_desc.is_empty() && self.state_desc.parse::<i64>().is_err() {
            ("rejected", self.state_desc.clone())
        } else {
            ("rejected", "未公开".to_string())
        }
    }
}

impl Bili {
    /// 拉取稿件列表（status=all），拿到 aid -> 稿件信息
    pub fn archive_map(&self, want: &[u64], cancel: &AtomicBool) -> HashMap<u64, ArchiveMeta> {
        let mut map: HashMap<u64, ArchiveMeta> = HashMap::new();
        if want.is_empty() {
            return map;
        }
        let mut pn = 1u64;
        let mut fetched = 0usize;
        loop {
            // 不按页数截断：按接口给的 count 翻到底（最多 400 页 = 2 万个稿件，防死循环）
            if cancel.load(Ordering::SeqCst) || pn > 400 {
                break;
            }
            let url = format!(
                "https://member.bilibili.com/x/web/archives?status=all&pn={}&ps=50",
                pn
            );
            let v = match self.get_api(
                &url,
                "https://member.bilibili.com/platform/upload-manager/article",
            ) {
                Ok(v) => v,
                Err(_) => {
                    std::thread::sleep(self.next_pause());
                    match self.get_api(
                        &url,
                        "https://member.bilibili.com/platform/upload-manager/article",
                    ) {
                        Ok(v) => v,
                        Err(_) => break,
                    }
                }
            };
            let empty = Vec::new();
            let list = v["data"]["arc_audits"].as_array().unwrap_or(&empty);
            let count = v["data"]["page"]["count"].as_u64().unwrap_or(0);
            for item in list {
                let a = &item["Archive"];
                let aid = num(&a["aid"]);
                if aid == 0 {
                    continue;
                }
                map.insert(
                    aid,
                    ArchiveMeta {
                        aid,
                        bvid: a["bvid"].as_str().unwrap_or("").to_string(),
                        title: a["title"].as_str().unwrap_or("").to_string(),
                        cover: https_cover(a["cover"].as_str().unwrap_or("")),
                        ptime: a["ptime"].as_i64().unwrap_or(0),
                        state: a["state"].as_i64().unwrap_or(0),
                        state_desc: a["state_desc"].as_str().unwrap_or("").to_string(),
                        only_self: a["is_only_self"].as_i64().unwrap_or(0) == 1,
                        reply: num(&item["stat"]["reply"]),
                        view: num(&item["stat"]["view"]),
                    },
                );
            }
            fetched += list.len();
            if list.is_empty() || fetched as u64 >= count {
                break;
            }
            pn += 1;
            std::thread::sleep(self.next_pause());
        }
        map
    }
}

/// 把评论行里的临时稿件信息，和稿件列表里的正式信息合并
pub fn merge_video_meta(videos: &mut [Video], map: &HashMap<u64, ArchiveMeta>) {
    for v in videos.iter_mut() {
        v.cover = https_cover(&v.cover);
        // 动态/专栏/音频不在视频稿件列表里，不能用视频稿件的信息覆盖它们：
        // 否则会被误标「已删除」、URL 被改成 av 地址、标题被套成 av 号。
        // 它们的状态/标题/原文/URL 由后面的动态原文那一步专门填充。
        // 注意空 kind 是老数据的「视频」，也要照常合并。
        if !v.kind.is_empty() && v.kind != "video" {
            continue;
        }
        match map.get(&v.aid) {
            Some(m) => {
                if !m.title.is_empty() {
                    v.title = m.title.clone();
                }
                if !m.bvid.is_empty() {
                    v.bvid = m.bvid.clone();
                    v.url = Video::url_for(&m.bvid, v.aid);
                }
                if !m.cover.is_empty() {
                    v.cover = m.cover.clone();
                }
                if m.ptime > 0 {
                    v.publish_time = m.ptime;
                    v.publish_time_text = ts_text(m.ptime);
                }
                if m.reply > 0 {
                    v.comment_total = m.reply;
                }
                let (st, text) = m.status();
                v.status = st.to_string();
                v.status_text = text;
            }
            None => {
                // 稿件列表里没有它 —— 已被删除（评论管理里仍然留着评论）
                v.status = "deleted".to_string();
                v.status_text = "已删除".to_string();
                if v.comment_total < v.comments.len() as u64 {
                    v.comment_total = v.comments.len() as u64;
                }
            }
        }
        if v.title.is_empty() {
            v.title = format!("av{}", v.aid);
        }
        if v.url.is_empty() {
            v.url = Video::url_for(&v.bvid, v.aid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(rpid: u64, oid: u64, root: u64, title: &str, cover: &str) -> CommentRow {
        CommentRow {
            rpid,
            oid,
            root,
            content: "测试评论".into(),
            ctime: 1_700_000_000,
            bvid: "BV1test".into(),
            title: title.into(),
            cover: cover.into(),
            ..Default::default()
        }
    }

    #[test]
    fn parses_location_pictures_and_vip() {
        let v = serde_json::json!({
            "rpid": 1,
            "oid": 100,
            "member": {
                "mid": 42,
                "uname": "甲",
                "avatar": "http://i0.hdslb.com/face.jpg",
                "level_info": { "current_level": 6 },
                "vip": { "vipStatus": 1, "vipType": 2 }
            },
            "content": {
                "message": "带图评论",
                "pictures": [{ "img_src": "http://i0.hdslb.com/p1.jpg" }]
            },
            "reply_control": { "location": "IP属地：上海" },
            "ctime": 1_700_000_000,
            "like": 3
        });
        let c = parse_comment(&v, 0).unwrap();
        assert_eq!(c.location, "上海");
        assert_eq!(c.pictures, vec!["https://i0.hdslb.com/p1.jpg"]);
        assert!(c.is_vip);
        assert_eq!(c.avatar, "https://i0.hdslb.com/face.jpg");
    }

    #[test]
    fn cookie_header_merges_fingerprint_once() {
        let mk = |cookie: &str| Bili {
            http: http_client().unwrap(),
            cookie: cookie.to_string(),
            buvid3: "4501E016-6EA7-95C2-8EAD-1F3C7A0B74F839389infoc".to_string(),
            buvid4: "06ACFA5B-AC1E".to_string(),
            bnut: 1_700_000_000,
            wbi: None,
            interval_min_ms: 0,
            interval_max_ms: 0,
        };

        let h = mk("SESSDATA=abc%2C123; bili_jct=ff").cookie_header();
        assert!(h.contains("SESSDATA=abc%2C123"));
        assert!(h.contains("buvid3=4501E016"), "{}", h);
        assert!(!h.contains("infocinfoc"), "infoc 不能重复：{}", h);
        assert_eq!(h.matches("buvid3=").count(), 1, "{}", h);
        assert!(h.contains("buvid4=06ACFA5B-AC1Einfoc"));
        assert!(h.contains("b_nut=1700000000"));

        // Cookie 里已经带了 buvid3 / b_nut 就不再追加
        let h2 = mk("buvid3=XYZinfoc; b_nut=123").cookie_header();
        assert_eq!(h2.matches("buvid3").count(), 1, "{}", h2);
        assert_eq!(h2.matches("b_nut").count(), 1, "{}", h2);
    }

    #[test]
    fn expired_vip_is_not_member() {
        // 过期用户的 vipType 仍是 1，但 vipStatus=0，不能标成大会员
        let expired = serde_json::json!({
            "mid": 1, "uname": "丙",
            "vip": { "vipStatus": 0, "vipType": 1, "accessStatus": 0, "label": { "text": "" } }
        });
        assert!(!is_vip_of(&expired));

        let active = serde_json::json!({ "vip": { "vipStatus": 1, "vipType": 1 } });
        assert!(is_vip_of(&active));

        // 没有 vip 字段时不能误判
        let anon = serde_json::json!({ "mid": 2, "uname": "丁" });
        assert!(!is_vip_of(&anon));

        // 不带 vipStatus 的接口退回看档位 + 标签文字
        let no_status_active = serde_json::json!({
            "vip": { "vipType": 2, "label": { "text": "年度大会员" } }
        });
        assert!(is_vip_of(&no_status_active));
        let no_status_expired = serde_json::json!({
            "vip": { "vipType": 2, "label": { "text": "" } }
        });
        assert!(!is_vip_of(&no_status_expired));
    }

    #[test]
    fn image_only_comment_is_kept() {
        let v = serde_json::json!({
            "rpid": 2,
            "member": { "mid": 1, "uname": "乙" },
            "content": { "message": "", "pictures": [{ "img_src": "http://i0.hdslb.com/p2.jpg" }] },
            "ctime": 1_700_000_100
        });
        let c = parse_comment(&v, 0).unwrap();
        assert!(c.content.is_empty());
        assert_eq!(c.pictures.len(), 1);
        assert!(!c.is_vip);
    }

    #[test]
    fn deleted_video_keeps_title_and_cover_from_comment_rows() {
        // 两个稿件：一个在稿件列表里，一个已经被删（列表里没有）
        let rows = vec![
            row(1, 100, 0, "还在的稿件", "http://i0.hdslb.com/a.jpg"),
            row(2, 200, 0, "被删掉的稿件", "http://i0.hdslb.com/b.jpg"),
        ];
        let mut videos = rows_to_videos(&rows, true, 9, "up");
        let mut map = HashMap::new();
        map.insert(
            100,
            ArchiveMeta {
                aid: 100,
                bvid: "BV1keep".into(),
                title: "还在的稿件（正式标题）".into(),
                cover: "https://i0.hdslb.com/a.jpg".into(),
                ptime: 1_600_000_000,
                state: 0,
                state_desc: "开放浏览".into(),
                only_self: false,
                reply: 42,
                view: 1000,
            },
        );
        let _ = 200u64;
        merge_video_meta(&mut videos, &map);

        let keep = videos.iter().find(|v| v.aid == 100).unwrap();
        assert_eq!(keep.status, "public");
        assert_eq!(keep.title, "还在的稿件（正式标题）");
        assert_eq!(keep.comment_total, 42);
        assert!(!keep.publish_time_text.is_empty());

        let gone = videos.iter().find(|v| v.aid == 200).unwrap();
        assert_eq!(gone.status, "deleted");
        assert_eq!(gone.status_text, "已删除");
        // 稿件列表里没有它，标题和封面来自评论行
        assert_eq!(gone.title, "被删掉的稿件");
        assert_eq!(gone.cover, "https://i0.hdslb.com/b.jpg");
        assert!(gone.is_hidden());
    }

    #[test]
    fn private_video_marked_as_hidden() {
        let mut videos = vec![Video {
            aid: 300,
            title: "自己可见".into(),
            ..Default::default()
        }];
        let mut map = HashMap::new();
        map.insert(
            300,
            ArchiveMeta {
                aid: 300,
                title: "自己可见".into(),
                state: -50,
                only_self: true,
                ..Default::default()
            },
        );
        merge_video_meta(&mut videos, &map);
        assert_eq!(videos[0].status, "private");
        assert_eq!(videos[0].status_text, "仅自己可见");
    }
}


const RISK_WAIT_SECONDS: u64 = 60;

fn retry_with_risk_wait<T>(
    mut request: impl FnMut() -> Result<T>,
    cancel: &AtomicBool,
    on_wait: &mut dyn FnMut(u64),
    on_tick: &mut dyn FnMut(u64),
    sleep: &mut dyn FnMut(Duration),
) -> Result<T> {
    loop {
        if cancel.load(Ordering::SeqCst) { return Err(anyhow!("已取消")); }
        match request() {
            Ok(value) => return Ok(value),
            Err(error) => {
                let message=error.to_string();
                let retryable=message.contains("-352") || message.contains("-412") || message.contains("-509")
                    || message.contains("超时") || message.contains("timed out");
                if !retryable { return Err(error); }
            }
        }
        on_wait(RISK_WAIT_SECONDS);
        on_tick(RISK_WAIT_SECONDS);
        // Zero means checking, never successful recovery. The caller closes its
        // dialog only after the original request returns successfully.
        for step in 1..=RISK_WAIT_SECONDS * 4 {
            if cancel.load(Ordering::SeqCst) { return Err(anyhow!("已取消")); }
            sleep(Duration::from_millis(250));
            if step % 4 == 0 { on_tick(RISK_WAIT_SECONDS - step / 4); }
        }
    }
}

#[cfg(test)]
mod risk_wait_tests {
    use super::*;
    use std::cell::RefCell;
    #[test]
    fn waits_sixty_seconds_and_rechecks_until_the_request_succeeds() {
        let cancel=AtomicBool::new(false);let events=RefCell::new(Vec::new());let mut requests=0;
        let result=retry_with_risk_wait(|| {
            requests+=1;
            if requests<3 {Err(anyhow!("-352"))} else {Ok(42)}
        },&cancel,&mut |n|events.borrow_mut().push(("wait",n)),
        &mut |n|events.borrow_mut().push(("tick",n)),&mut |_|{}).unwrap();
        assert_eq!(result,42);assert_eq!(requests,3);
        assert_eq!(events.borrow().iter().filter(|e|**e==("wait",60)).count(),2);
        assert_eq!(events.borrow().iter().filter(|e|**e==("tick",0)).count(),2);
    }
    #[test]
    fn cancellation_and_non_retryable_errors_stop_without_more_waiting() {
        let cancel=AtomicBool::new(false);
        let error=retry_with_risk_wait::<()>(||Err(anyhow!("-352")),&cancel,&mut |_|{},&mut |_|{},
            &mut |_|cancel.store(true,Ordering::SeqCst)).unwrap_err();
        assert!(error.to_string().contains("已取消"));
        let mut waits=0;let cancel=AtomicBool::new(false);
        assert!(retry_with_risk_wait::<()>(||Err(anyhow!("invalid request")),&cancel,&mut |_|waits+=1,&mut |_|{},&mut |_|{}).is_err());
        assert_eq!(waits,0);
    }
}
