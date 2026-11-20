//! 内置的本地 Web 服务：把整合好的评论数据用网页列出来，
//! 同时提供 /api/data.json 接口给别的程序消费。
//! 只用标准库的 TcpListener，不额外引 HTTP 框架。

use crate::export;
use crate::model::ArchiveDoc;
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const INDEX_HTML: &str = include_str!("../web/index.html");

/// 在 127.0.0.1 上起服务，只监听指定端口，占用时报错
pub fn spawn(doc: Arc<Mutex<ArchiveDoc>>, base_port: u16) -> Result<u16, String> {
    spawn_with_stop(doc, base_port, Arc::new(AtomicBool::new(false)))
}

/// 和 spawn 一样，但外部置位 stop 后服务线程会退出、释放端口。
/// 监听器设为非阻塞，避免 stop 之后还卡在 incoming() 上。
pub fn spawn_with_stop(
    doc: Arc<Mutex<ArchiveDoc>>,
    base_port: u16,
    stop: Arc<AtomicBool>,
) -> Result<u16, String> {
    let listener = TcpListener::bind(("127.0.0.1", base_port)).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            format!("端口 {} 已被占用，请修改端口后重试", base_port)
        } else {
            format!("无法监听端口 {}：{}", base_port, e)
        }
    })?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let _ = listener.set_nonblocking(true);

    std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((s, _)) => {
                    let doc = Arc::clone(&doc);
                    std::thread::spawn(move || handle(s, doc));
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(_) => break,
            }
        }
    });
    Ok(port)
}

fn handle(mut stream: TcpStream, doc: Arc<Mutex<ArchiveDoc>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
    let rd = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut reader = BufReader::new(rd);
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    // 读完请求头，避免连接状态混乱
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).unwrap_or(0) == 0 {
            break;
        }
        if h == "\r\n" || h == "\n" || h.is_empty() {
            break;
        }
    }
    if method != "GET" && method != "HEAD" {
        respond(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain; charset=utf-8",
            b"only GET",
        );
        return;
    }

    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.clone(), String::new()),
    };
    let param = |key: &str| -> Option<String> {
        query.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            if k == key {
                Some(
                    urlencoding::decode(v)
                        .map(|s| s.into_owned())
                        .unwrap_or_else(|_| v.to_string()),
                )
            } else {
                None
            }
        })
    };

    match path.as_str() {
        "/" | "/index.html" => respond(
            &mut stream,
            "200 OK",
            "text/html; charset=utf-8",
            INDEX_HTML.as_bytes(),
        ),
        "/health" => respond(&mut stream, "200 OK", "text/plain; charset=utf-8", b"ok"),
        "/api/data.json" => {
            let body = {
                let d = doc.lock().unwrap();
                export::to_json(&d).unwrap_or_else(|_| "{}".into())
            };
            respond(
                &mut stream,
                "200 OK",
                "application/json; charset=utf-8",
                body.as_bytes(),
            );
        }
        "/api/videos" => {
            let body = {
                let d = doc.lock().unwrap();
                let list: Vec<serde_json::Value> = d
                    .videos
                    .iter()
                    .map(|v| {
                        json!({
                            "aid": v.aid,
                            "bvid": v.bvid,
                            "title": v.display_title(),
                            "cover": v.cover,
                            "url": v.url,
                            "status": v.status,
                            "statusText": v.status_text,
                            "publishTime": v.publish_time_text,
                            "commentTotal": v.comment_total,
                            "fetched": v.comments.len(),
                            "replies": v.total_replies(),
                        })
                    })
                    .collect();
                serde_json::to_string_pretty(&json!({
                    "stats": d.stats,
                    "exportedAt": d.meta.exported_at,
                    "videos": list,
                }))
                .unwrap_or_else(|_| "{}".into())
            };
            respond(
                &mut stream,
                "200 OK",
                "application/json; charset=utf-8",
                body.as_bytes(),
            );
        }
        "/api/video" => {
            let aid: u64 = param("aid").and_then(|s| s.parse().ok()).unwrap_or(0);
            let body = {
                let d = doc.lock().unwrap();
                match d.videos.iter().find(|v| v.aid == aid) {
                    Some(v) => serde_json::to_string(v).unwrap_or_else(|_| "{}".into()),
                    None => "{\"error\":\"not found\"}".to_string(),
                }
            };
            respond(
                &mut stream,
                "200 OK",
                "application/json; charset=utf-8",
                body.as_bytes(),
            );
        }
        "/api/comments" => {
            // /api/comments?aid=123&q=关键词  （评论正文、昵称、楼中楼都会命中）
            let aid: u64 = param("aid").and_then(|s| s.parse().ok()).unwrap_or(0);
            let q = param("q").unwrap_or_default().to_lowercase();
            let body = {
                let d = doc.lock().unwrap();
                let mut out = Vec::new();
                for v in d.videos.iter().filter(|v| aid == 0 || v.aid == aid) {
                    for c in &v.comments {
                        let reply_hit = c
                            .replies
                            .iter()
                            .any(|r| r.content.to_lowercase().contains(&q));
                        if !q.is_empty()
                            && !c.content.to_lowercase().contains(&q)
                            && !c.uname.to_lowercase().contains(&q)
                            && !reply_hit
                        {
                            continue;
                        }
                        out.push(json!({
                            "aid": v.aid,
                            "title": v.display_title(),
                            "rpid": c.rpid,
                            "mid": c.mid,
                            "uname": c.uname,
                            "avatar": c.avatar,
                            "isVip": c.is_vip,
                            "location": c.location,
                            "pictures": c.pictures,
                            "content": c.content,
                            "like": c.like,
                            "ctime": c.ctime_text,
                            "isUp": c.is_up,
                            "replies": c.replies.len(),
                            "repliesPreview": c.replies.iter().take(3).map(|r| json!({
                                "rpid": r.rpid,
                                "mid": r.mid,
                                "uname": r.uname,
                                "avatar": r.avatar,
                                "isVip": r.is_vip,
                                "location": r.location,
                                "content": r.content,
                                "like": r.like,
                                "ctime": r.ctime_text,
                            })).collect::<Vec<_>>(),
                        }));
                    }
                }
                serde_json::to_string_pretty(&json!({ "count": out.len(), "items": out }))
                    .unwrap_or_else(|_| "{}".into())
            };
            respond(
                &mut stream,
                "200 OK",
                "application/json; charset=utf-8",
                body.as_bytes(),
            );
        }
        _ => respond(
            &mut stream,
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"not found",
        ),
    }
}

fn respond(stream: &mut TcpStream, status: &str, ctype: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
         Access-Control-Allow-Origin: *\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        status,
        ctype,
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

#[cfg(test)]
mod port_tests {
    use super::*;
    #[test]
    fn occupied_port_does_not_advance() {
        let occupied = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = occupied.local_addr().unwrap().port();
        let error = spawn(Arc::new(Mutex::new(ArchiveDoc::default())), port).unwrap_err();
        assert!(error.contains(&port.to_string()));
        assert!(error.contains("已被占用"));
    }
    #[test]
    fn reports_actual_port_and_releases_listener() {
        let stop = Arc::new(AtomicBool::new(false));
        let port =
            spawn_with_stop(Arc::new(Mutex::new(ArchiveDoc::default())), 0, stop.clone()).unwrap();
        assert!(port > 0);
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut text = String::new();
        use std::io::Read;
        stream.read_to_string(&mut text).unwrap();
        assert!(text.ends_with("ok"));
        stop.store(true, Ordering::SeqCst);
        for _ in 0..100 {
            if TcpListener::bind(("127.0.0.1", port)).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("listener was not released");
    }
}
