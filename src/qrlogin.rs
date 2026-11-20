//! 扫码登录：拿二维码 → 轮询扫码状态 → 成功时把整套 Cookie 抓下来。
//!
//! 相比手抄 SESSDATA，扫码能一次拿到 `SESSDATA / bili_jct / DedeUserID /
//! DedeUserID__ckMd5 / sid` 全套，再叠加 `buvid3 / buvid4` 设备指纹，
//! 接口校验（-352）基本就不会再拦了。

use crate::bili;
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::time::Duration;

const GENERATE: &str =
    "https://passport.bilibili.com/x/passport-login/web/qrcode/generate?source=main-fe-header";
const POLL: &str = "https://passport.bilibili.com/x/passport-login/web/qrcode/poll";

pub enum Poll {
    /// 还没扫
    Waiting,
    /// 扫了，等手机上点确认
    Scanned,
    /// 二维码过期，需要重新生成
    Expired,
    /// 登录成功，带整套 Cookie
    Done { cookie: String, mid: u64 },
}

pub struct QrLogin {
    http: reqwest::blocking::Client,
    pub buvid3: String,
    pub buvid4: String,
}

impl QrLogin {
    pub fn new() -> Result<Self> {
        let http = bili::http_client()?;
        let (buvid3, buvid4) = bili::fetch_fingerprint(&http);
        Ok(Self {
            http,
            buvid3,
            buvid4,
        })
    }

    /// 返回 (二维码内容, qrcode_key)
    pub fn generate(&self) -> Result<(String, String)> {
        let v: Value = self
            .http
            .get(GENERATE)
            .header("Referer", "https://www.bilibili.com/")
            .header("Accept", "application/json, text/plain, */*")
            .send()?
            .json()?;
        check(&v)?;
        let url = v["data"]["url"].as_str().unwrap_or("").to_string();
        let key = v["data"]["qrcode_key"].as_str().unwrap_or("").to_string();
        if url.is_empty() || key.is_empty() {
            return Err(anyhow!("二维码接口没返回内容"));
        }
        Ok((url, key))
    }

    /// 查一次扫码状态
    pub fn poll(&self, key: &str) -> Result<Poll> {
        let mut req = self
            .http
            .get(format!("{}?qrcode_key={}&source=main-fe-header", POLL, key))
            .header("Referer", "https://www.bilibili.com/")
            .header("Accept", "application/json, text/plain, */*");
        let cookie = self.cookie_header();
        if !cookie.is_empty() {
            req = req.header("Cookie", cookie);
        }
        let resp = req.send()?;
        // 登录成功时凭据在 Set-Cookie 里，必须在读完 body 之前取出来
        let set_cookies: Vec<String> = resp
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .collect();
        let body = resp.text()?;
        let v: Value = serde_json::from_str(&body).map_err(|_| anyhow!("扫码接口返回异常"))?;
        check(&v)?;

        let code = v["data"]["code"].as_i64().unwrap_or(-1);
        match code {
            0 => {
                let cookie = collect_cookie(&set_cookies, v["data"]["url"].as_str().unwrap_or(""));
                if !cookie.contains("SESSDATA=") {
                    return Err(anyhow!("登录成功但没拿到 SESSDATA，请重试"));
                }
                let mid = pick(&cookie, "DedeUserID").parse::<u64>().unwrap_or(0);
                Ok(Poll::Done { cookie, mid })
            }
            86101 => Ok(Poll::Waiting),
            86090 => Ok(Poll::Scanned),
            86038 => Ok(Poll::Expired),
            other => Err(anyhow!(
                "{}（code {}）",
                v["data"]["message"].as_str().unwrap_or("扫码状态未知"),
                other
            )),
        }
    }

    fn cookie_header(&self) -> String {
        let mut parts = Vec::new();
        if !self.buvid3.is_empty() {
            parts.push(format!("buvid3={}", infoc(&self.buvid3)));
        }
        if !self.buvid4.is_empty() {
            parts.push(format!("buvid4={}", infoc(&self.buvid4)));
        }
        parts.join("; ")
    }
}

fn check(v: &Value) -> Result<()> {
    let code = v.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
    if code != 0 {
        return Err(anyhow!(
            "{}（code {}）",
            v.get("message").and_then(|m| m.as_str()).unwrap_or("请求失败"),
            code
        ));
    }
    Ok(())
}

fn infoc(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() || t.ends_with("infoc") {
        t.to_string()
    } else {
        format!("{}infoc", t)
    }
}

/// 从 Set-Cookie 和登录成功时返回的跳转 URL 里拼出整套 Cookie
fn collect_cookie(set_cookies: &[String], redirect: &str) -> String {
    let wanted = [
        "SESSDATA",
        "bili_jct",
        "DedeUserID",
        "DedeUserID__ckMd5",
        "sid",
        "buvid3",
        "buvid4",
    ];
    let mut found: Vec<(String, String)> = Vec::new();
    for name in wanted {
        let mut val = String::new();
        // Set-Cookie 优先（服务端真下发的值）
        for sc in set_cookies {
            if let Some(v) = cookie_from_set_cookie(sc, name) {
                val = v;
                break;
            }
        }
        if val.is_empty() {
            if let Some(v) = cookie_from_url(redirect, name) {
                val = v;
            }
        }
        if !val.is_empty() {
            found.push((name.to_string(), val));
        }
    }
    found
        .into_iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("; ")
}

fn cookie_from_set_cookie(header: &str, name: &str) -> Option<String> {
    let first = header.split(';').next()?.trim();
    let (k, v) = first.split_once('=')?;
    if k.trim() == name && !v.trim().is_empty() {
        Some(v.trim().to_string())
    } else {
        None
    }
}

fn cookie_from_url(url: &str, name: &str) -> Option<String> {
    let query = url.split('?').nth(1)?;
    let query = query.split('#').next()?;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=')?;
        if k == name && !v.is_empty() {
            return Some(v.to_string());
        }
    }
    None
}

fn pick(cookie: &str, name: &str) -> String {
    cookie
        .split(';')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| k.trim() == name)
        .map(|(_, v)| v.trim().to_string())
        .unwrap_or_default()
}

/// 把二维码内容渲染成 RGBA 像素（给 Slint 的 Image 用），返回 (像素, 宽, 高)
pub fn qr_rgba(text: &str, px: u32) -> Option<(Vec<u8>, u32, u32)> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let n = code.width() as u32;
    let cols = code.to_colors();
    let quiet = 2u32;
    let total = n + quiet * 2;
    let scale = (px / total).max(1);
    let size = total * scale;
    let mut buf = vec![255u8; (size * size * 4) as usize];
    for y in 0..n {
        for x in 0..n {
            let dark = cols[(y * n + x) as usize] == qrcode::Color::Dark;
            if !dark {
                continue;
            }
            let px0 = (x + quiet) * scale;
            let py0 = (y + quiet) * scale;
            for dy in 0..scale {
                for dx in 0..scale {
                    let idx = (((py0 + dy) * size + px0 + dx) * 4) as usize;
                    buf[idx] = 17; // #111827，比纯黑柔和一点，扫码识别率一样
                    buf[idx + 1] = 24;
                    buf[idx + 2] = 39;
                    buf[idx + 3] = 255;
                }
            }
        }
    }
    Some((buf, size, size))
}

/// 终端里显示的二维码（命令行 login 用）
pub fn qr_ascii(text: &str) -> Option<String> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let n = code.width();
    let cols = code.to_colors();
    let mut out = String::new();
    let blank = "  ".repeat(n + 2);
    out.push_str(&blank);
    out.push('\n');
    for y in 0..n {
        out.push_str("  ");
        for x in 0..n {
            out.push_str(if cols[y * n + x] == qrcode::Color::Dark {
                "██"
            } else {
                "  "
            });
        }
        out.push_str("  \n");
    }
    out.push_str(&blank);
    out.push('\n');
    Some(out)
}

/// 轮询时给界面用的状态文案
pub fn status_text(p: &Poll) -> &'static str {
    match p {
        Poll::Waiting => "等待扫码…请用 B 站手机客户端扫一扫",
        Poll::Scanned => "已扫码，请在手机上点「确认登录」",
        Poll::Expired => "二维码已过期，正在换一张…",
        Poll::Done { .. } => "登录成功",
    }
}

/// 默认的两次轮询间隔（B 站二维码有效期约 3 分钟）
pub const POLL_INTERVAL: Duration = Duration::from_millis(2000);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_cookie_from_headers_and_url() {
        let set_cookies = vec![
            "SESSDATA=abc%2C123; Path=/; Domain=.bilibili.com".to_string(),
            "bili_jct=deadbeef; Path=/".to_string(),
        ];
        let redirect =
            "https://www.bilibili.com/?DedeUserID=42&DedeUserID__ckMd5=ff&sid=xyz&buvid3=X1infoc";
        let c = collect_cookie(&set_cookies, redirect);
        assert!(c.contains("SESSDATA=abc%2C123"));
        assert!(c.contains("bili_jct=deadbeef"));
        assert!(c.contains("DedeUserID=42"));
        assert!(c.contains("sid=xyz"));
        assert!(c.contains("buvid3=X1infoc"));
        assert_eq!(pick(&c, "DedeUserID"), "42");
    }

    #[test]
    fn infoc_suffix_is_not_duplicated() {
        assert_eq!(infoc("ABCinfoc"), "ABCinfoc");
        assert_eq!(infoc("ABC"), "ABCinfoc");
    }

    #[test]
    fn qr_pixels_and_ascii_are_produced() {
        let (px, w, h) = qr_rgba("https://example.com/qr", 320).unwrap();
        assert_eq!(w, h);
        assert_eq!(px.len(), (w * h * 4) as usize);
        // 左上角是留白，必须为白
        assert_eq!(&px[0..4], &[255, 255, 255, 255]);
        let art = qr_ascii("https://example.com/qr").unwrap();
        assert!(art.contains("██"));
    }
}
