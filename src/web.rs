//! 内置的本地 Web 服务：把整合好的评论数据用网页列出来，
//! 同时提供 /api/data.json 接口给别的程序消费。
//! 只用标准库的 TcpListener，不额外引 HTTP 框架。

use crate::export;
use crate::messages;
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
    spawn_with_messages(
        doc,
        Arc::new(Mutex::new(messages::Store::default())),
        base_port,
        stop,
    )
}

pub fn spawn_with_messages(
    doc: Arc<Mutex<ArchiveDoc>>,
    messages: Arc<Mutex<messages::Store>>,
    base_port: u16,
    stop: Arc<AtomicBool>,
) -> Result<u16, String> {
    spawn_with_account(doc, messages, Arc::new(Mutex::new(crate::account_ui::PublicAccount::default())), base_port, stop)
}

pub fn spawn_with_account(
    doc: Arc<Mutex<ArchiveDoc>>,
    messages: Arc<Mutex<messages::Store>>,
    account: Arc<Mutex<crate::account_ui::PublicAccount>>,
    base_port: u16,
    stop: Arc<AtomicBool>,
) -> Result<u16, String> {
    spawn_with_subjects(doc, messages, account, base_port, stop, Arc::new(|_| None))
}

pub fn spawn_with_subjects(
    doc: Arc<Mutex<ArchiveDoc>>,
    messages: Arc<Mutex<messages::Store>>,
    account: Arc<Mutex<crate::account_ui::PublicAccount>>,
    base_port: u16,
    stop: Arc<AtomicBool>,
    credentials: crate::message_subjects::Credentials,
) -> Result<u16, String> {
    spawn_with_profile_pacing(doc, messages, account, base_port, stop, credentials, Arc::new(|| (1800, 2400)))
}

pub fn spawn_with_profile_pacing(
    doc: Arc<Mutex<ArchiveDoc>>,
    messages: Arc<Mutex<messages::Store>>,
    account: Arc<Mutex<crate::account_ui::PublicAccount>>,
    base_port: u16,
    stop: Arc<AtomicBool>,
    credentials: crate::message_subjects::Credentials,
    pacing: crate::message_profiles::Pacing,
) -> Result<u16, String> {
    let profiles = Arc::new(crate::message_profiles::Service::default());
    let subjects = Arc::new(crate::message_subjects::Service::default());
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
                    let messages = messages.clone();
                    let account = account.clone();
                    let subjects = subjects.clone(); let credentials = credentials.clone();
                    let profiles = profiles.clone(); let pacing = pacing.clone();
                    std::thread::spawn(move || handle(s, doc, messages, account, subjects, credentials, profiles, pacing));
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

fn handle(
    mut stream: TcpStream,
    doc: Arc<Mutex<ArchiveDoc>>,
    messages: Arc<Mutex<messages::Store>>,
    account: Arc<Mutex<crate::account_ui::PublicAccount>>,
    subjects: Arc<crate::message_subjects::Service>,
    credentials: crate::message_subjects::Credentials,
    profiles: Arc<crate::message_profiles::Service>,
    pacing: crate::message_profiles::Pacing,
) {
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
    let mut host = String::new();
    let mut origin = String::new();
    // 读完请求头，避免连接状态混乱
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).unwrap_or(0) == 0 {
            break;
        }
        if let Some((key, value)) = h.split_once(':') {
            if key.eq_ignore_ascii_case("host") {
                host = value.trim().into()
            }
            if key.eq_ignore_ascii_case("origin") {
                origin = value.trim().into()
            }
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

    if path == "/api/account" || path == "/messages" || path == "/messages/" || path.starts_with("/api/messages/") || path.starts_with("/api/archive-resources/") || path == "/api/archive.json" {
        let local_host = host == "127.0.0.1"
            || host
                .strip_prefix("127.0.0.1:")
                .is_some_and(|p| p.parse::<u16>().is_ok());
        if !local_host || (!origin.is_empty() && origin != format!("http://{}", host)) {
            respond_private(
                &mut stream,
                "403 Forbidden",
                "text/plain",
                b"same origin only",
                method == "HEAD",
            );
            return;
        }
        if path == "/messages" || path == "/messages/" {
            respond_private(
                &mut stream,
                "200 OK",
                "text/html; charset=utf-8",
                INDEX_HTML.as_bytes(),
                method == "HEAD",
            );
            return;
        }
        if path == "/api/account" {
            let body = serde_json::to_vec(&*account.lock().unwrap()).unwrap();
            respond_private(&mut stream, "200 OK", "application/json; charset=utf-8", &body, method == "HEAD");
            return;
        }
        if path=="/api/archive.json" {let body=export::to_json(&doc.lock().unwrap()).unwrap_or_else(|_|"{}".into());respond_private(&mut stream,"200 OK","application/json; charset=utf-8",body.as_bytes(),method=="HEAD");return;}
        if let Some(key)=path.strip_prefix("/api/archive-resources/") {
            let (message_doc,resource_root)={let state=messages.lock().unwrap();(state.doc.clone(),if state.root.as_os_str().is_empty(){crate::archive::comment_root(&doc.lock().unwrap())}else{crate::archive::storage_root(&state.root)})};
            let uid=message_doc.account_uid.clone();let policy=crate::archive::ResourcePolicy::load();
            let mut resources=crate::archive::message_resources(&message_doc,&resource_root,policy.clone());
            {let snapshot=doc.lock().unwrap();resources.extend(crate::archive::from_comments(&snapshot,crate::archive::comment_account(&snapshot),&resource_root,policy).resources);}
            let file=resources.get(key).and_then(|r|r["path"].as_str()).and_then(|p|crate::archive::resource_file(&resource_root,p));
            if messages.lock().unwrap().doc.account_uid!=uid {respond_private(&mut stream,"409 Conflict","text/plain",b"account changed",method=="HEAD");return;}
            match file.and_then(|p|std::fs::File::open(&p).ok().map(|f|(f,crate::messages::media_type(&p)))) {
                Some((file,mime))=>respond_private_file(&mut stream,file,mime,method=="HEAD"),
                None=>respond_private(&mut stream,"404 Not Found","text/plain",b"not found",method=="HEAD"),
            }return;
        }
        let state = messages.lock().unwrap();
        if path == "/api/messages/profile" {
            let uid = state.doc.account_uid.clone();
            if param("account_uid").as_deref() != Some(uid.as_str()) {
                respond_private(&mut stream, "409 Conflict", "text/plain", b"account changed", method == "HEAD"); return;
            }
            let local = crate::message_profiles::local_profiles(&state.doc);
            let found = param("uid").and_then(|key| local.get(&key).cloned());
            let Some(profile) = found else {
                respond_private(&mut stream, "400 Bad Request", "text/plain", b"unknown archived user", method == "HEAD"); return;
            };
            drop(state);
            let profile = if method == "HEAD" || crate::library::offline() { profile } else { profiles.lookup(&uid, profile, param("retry").as_deref() == Some("1"), &credentials, &pacing) };
            if messages.lock().unwrap().doc.account_uid != uid {
                respond_private(&mut stream, "409 Conflict", "text/plain", b"account changed", method == "HEAD"); return;
            }
            let body = json!({"account_uid":uid,"profile":profile}).to_string();
            respond_private(&mut stream, "200 OK", "application/json; charset=utf-8", body.as_bytes(), method == "HEAD"); return;
        }
        if path == "/api/messages/subjects" {
            let uid = state.doc.account_uid.clone();
            if param("account_uid").as_deref() != Some(uid.as_str()) {
                respond_private(&mut stream,"409 Conflict","text/plain",b"account changed",method=="HEAD");return;
            }
            let keys: Vec<String> = param("keys").unwrap_or_default().split(',').filter(|s|!s.is_empty()).map(str::to_owned).collect();
            if keys.is_empty() || keys.len()>2 {respond_private(&mut stream,"400 Bad Request","text/plain",b"one or two subject keys required",method=="HEAD");return;}
            let message_doc = state.doc.clone(); drop(state);
            let local = crate::message_subjects::local_subjects(&message_doc, &doc.lock().unwrap());
            if keys.iter().any(|key|!local.contains_key(key)) {respond_private(&mut stream,"400 Bad Request","text/plain",b"unknown subject",method=="HEAD");return;}
            let retry=param("retry").as_deref()==Some("1");
            let items: Vec<_> = keys.iter().map(|key| {
                let subject=local[key].clone();
                if method=="HEAD" || crate::library::offline() {subject} else {subjects.lookup(&uid,subject,retry,&credentials)}
            }).collect();
            if messages.lock().unwrap().doc.account_uid!=uid {respond_private(&mut stream,"409 Conflict","text/plain",b"account changed",method=="HEAD");return;}
            let body=json!({"account_uid":uid,"subjects":items}).to_string();
            respond_private(&mut stream,"200 OK","application/json; charset=utf-8",body.as_bytes(),method=="HEAD");return;
        }
        if path == "/api/messages/summary" {
            if param("identity").as_deref() == Some("1") {
                let body = json!({"account_uid": state.doc.account_uid}).to_string();
                drop(state);
                respond_private(&mut stream, "200 OK", "application/json; charset=utf-8", body.as_bytes(), method == "HEAD");
                return;
            }
            // Build session and resource metadata without blocking list requests on the archive mutex.
            let message_doc = state.doc.clone();
            let resource_root = crate::archive::storage_root(&state.root);
            drop(state);
            let mut view = if crate::library::offline() {
                message_doc.summary()
            } else {
                profiles.summary(&message_doc)
            };
            view["resource_images"] = crate::archive::resource_images(
                &crate::archive::message_resources(&message_doc, &resource_root, crate::archive::ResourcePolicy::load()),
            );
            let body = view.to_string();
            respond_private(
                &mut stream,
                "200 OK",
                "application/json; charset=utf-8",
                body.as_bytes(),
                method == "HEAD",
            );
            return;
        }
        if path == "/api/messages/groups" {
            let ty=param("type").unwrap_or_default();let mode=param("group_by").unwrap_or_default();
            let query=param("q").unwrap_or_default();let ascending=param("order").is_some_and(|v|v=="old");
            let offset=param("offset").and_then(|s|s.parse::<usize>().ok()).unwrap_or(0);
            let limit=param("limit").and_then(|s|s.parse::<usize>().ok()).unwrap_or(50).clamp(1,200);
            let sort=param("sort").unwrap_or_else(||"time".into());
            match crate::message_groups::query(&state.doc,&ty,&mode,&query,ascending).and_then(|mut groups|{crate::message_groups::sort_groups(&mut groups,&sort)?;Ok(groups)}) {
                Ok(groups)=>{
                    let body=json!({"account_uid":state.doc.account_uid,"total":groups.len(),"total_items":groups.iter().map(|g|g.items.len()).sum::<usize>(),"groups":groups.iter().skip(offset).take(limit).map(|g|g.summary()).collect::<Vec<_>>()}).to_string();
                    respond_private(&mut stream,"200 OK","application/json; charset=utf-8",body.as_bytes(),method=="HEAD");
                },
                Err(_)=>respond_private(&mut stream,"400 Bad Request","text/plain",b"invalid grouping",method=="HEAD"),
            }
            return;
        }
        if path == "/api/messages/items" {
            let ty = param("type").unwrap_or_else(|| "all".into());
            let session = param("session").unwrap_or_default();
            let q = param("q").unwrap_or_default();
            if ty != "all" && !messages::VIEW_TYPES.contains(&ty.as_str()) {
                respond_private(
                    &mut stream,
                    "400 Bad Request",
                    "text/plain",
                    b"invalid type",
                    method == "HEAD",
                );
                return;
            }
            let offset = param("offset")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(0);
            let limit = param("limit")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(50)
                .clamp(1, 200);
            if param("group_by").is_some() || param("group_key").is_some() {
                let mode=param("group_by").unwrap_or_default();let key=param("group_key").unwrap_or_default();
                if key.is_empty(){respond_private(&mut stream,"400 Bad Request","text/plain",b"missing group key",method=="HEAD");return;}
                match crate::message_groups::query(&state.doc,&ty,&mode,&q,param("order").is_some_and(|v|v=="old")) {
                    Ok(groups)=>{
                        let items=groups.into_iter().find(|g|g.key==key).map(|g|g.items).unwrap_or_default();
                        let body=json!({"account_uid":state.doc.account_uid,"total":items.len(),"items":items.into_iter().skip(offset).take(limit).map(crate::message_subjects::summary).collect::<Vec<_>>()}).to_string();
                        respond_private(&mut stream,"200 OK","application/json; charset=utf-8",body.as_bytes(),method=="HEAD");
                    },
                    Err(_)=>respond_private(&mut stream,"400 Bad Request","text/plain",b"invalid grouping",method=="HEAD"),
                }
                return;
            }
            let include_groups = ty == "private" && param("include_groups").as_deref() == Some("1");
            let list: Vec<_> = state.doc.query_ordered(
                if include_groups { "all" } else { &ty }, &session, &q,
                param("order").as_deref().unwrap_or("new"),
            ).into_iter().filter(|item| !include_groups || ["private", "groups"].contains(&messages::item_type(item))).collect();
            let total = list.len();
            let page: Vec<_> = list.into_iter().skip(offset).take(limit).collect();
            let known = if page.iter().any(|i| i.ty == "private") {
                if crate::library::offline(){crate::message_profiles::local_profiles(&state.doc)}else{profiles.snapshot(&state.doc.account_uid, crate::message_profiles::local_profiles(&state.doc))}
            } else { Default::default() };
            let view=json!({"account_uid":state.doc.account_uid,"total":total,"items":page.into_iter().map(|i| crate::message_profiles::item_summary(i, &known)).collect::<Vec<_>>()});
            drop(state);
            let body=view.to_string();
            respond_private(
                &mut stream,
                "200 OK",
                "application/json; charset=utf-8",
                body.as_bytes(),
                method == "HEAD",
            );
            return;
        }

        if path == "/api/messages/likes" {
            let card_id = param("card_id").unwrap_or_default();
            let q = param("q").unwrap_or_default();
            let offset = param("offset")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(0);
            let limit = param("limit")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(50)
                .clamp(1, 200);
            if let Some(item) = state
                .doc
                .items
                .iter()
                .find(|i| i.ty == "likes" && i.id == card_id)
            {
                let users =
                    messages::like_users(item, &q, param("order").is_some_and(|s| s == "old"));
                let body=json!({"account_uid":state.doc.account_uid,"card_id":card_id,"like_count":item.like_count,"archived_count":item.like_users.len(),"like_detail":item.like_detail,"notice":messages::like_status(item),"total":users.len(),"items":users.into_iter().skip(offset).take(limit).collect::<Vec<_>>()}).to_string();
                respond_private(
                    &mut stream,
                    "200 OK",
                    "application/json; charset=utf-8",
                    body.as_bytes(),
                    method == "HEAD",
                );
                return;
            }
            respond_private(
                &mut stream,
                "404 Not Found",
                "text/plain",
                b"like notification not found",
                method == "HEAD",
            );
            return;
        }
        if let Some(key) = path.strip_prefix("/api/messages/attachments/") {
            if let Some(file) = messages::attachment_file(&state, key) {
                let ctype = messages::media_type(&file);
                drop(state);
                if let Ok(file) = std::fs::File::open(file) {
                    respond_private_file(&mut stream, file, ctype, method == "HEAD");
                    return;
                }
            }
            respond_private(
                &mut stream,
                "404 Not Found",
                "text/plain",
                b"attachment not found",
                method == "HEAD",
            );
            return;
        }
        respond_private(
            &mut stream,
            "404 Not Found",
            "text/plain",
            b"not found",
            method == "HEAD",
        );
        return;
    }
    match path.as_str() {
        "/assets/workspaces.css" => respond_private(
            &mut stream,
            "200 OK",
            "text/css; charset=utf-8",
            include_bytes!("../web/workspaces.css"),
            method == "HEAD",
        ),
        "/assets/comments.js" => respond_private(
            &mut stream,
            "200 OK",
            "text/javascript; charset=utf-8",
            include_bytes!("../web/comments.js"),
            method == "HEAD",
        ),
        "/assets/messages.js" => respond_private(
            &mut stream,
            "200 OK",
            "text/javascript; charset=utf-8",
            include_bytes!("../web/messages.js"),
            method == "HEAD",
        ),
        "/assets/noface.jpg" => respond(&mut stream, "200 OK", "image/jpeg", include_bytes!("../assets/noface.jpg")),
        "/assets/archive-images.js" => respond(&mut stream, "200 OK", "text/javascript; charset=utf-8", include_bytes!("../web/archive-images.js")),
        "/assets/account.js" => respond(&mut stream, "200 OK", "text/javascript; charset=utf-8", include_bytes!("../web/account.js")),
        "/assets/workspaces.js" => respond_private(
            &mut stream,
            "200 OK",
            "text/javascript; charset=utf-8",
            include_bytes!("../web/workspaces.js"),
            method == "HEAD",
        ),

        "/" | "/index.html" => respond(
            &mut stream,
            "200 OK",
            "text/html; charset=utf-8",
            INDEX_HTML.as_bytes(),
        ),
        "/health" => respond(&mut stream, "200 OK", "text/plain; charset=utf-8", b"ok"),
        "/api/archive.json" => {
            let body=export::to_json(&doc.lock().unwrap()).unwrap_or_else(|_|"{}".into());
            respond_private(&mut stream,"200 OK","application/json; charset=utf-8",body.as_bytes(),method=="HEAD");
        }
        "/api/data.json" => {
            let body = {
                let d = doc.lock().unwrap();
                let mut view=serde_json::to_value(&*d).unwrap_or_default();crate::archive::string_ids(&mut view);
                view["resource_images"]=crate::archive::resource_images(&crate::archive::from_comments(&d,crate::archive::comment_account(&d),&crate::archive::comment_root(&d),crate::archive::ResourcePolicy::load()).resources);
                crate::archive::localize_view(&mut view,&crate::archive::comment_root(&d),&crate::archive::comment_account(&d).to_string());
                view.to_string()
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
                            "type": v.r#type,
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

fn offline_policy(ctype:&str)->&'static str {
    if ctype.starts_with("text/html")&&crate::library::offline(){"Content-Security-Policy: img-src 'self' data: https://hdslb.com https://*.hdslb.com; media-src 'self' data: blob:; connect-src 'self'\r\n"}else{""}
}
fn respond(stream: &mut TcpStream, status: &str, ctype: &str, body: &[u8]) {
    let policy=offline_policy(ctype);
    let head = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
         Access-Control-Allow-Origin: *\r\n{policy}Cache-Control: no-store\r\nConnection: close\r\n\r\n",
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

// Account archives deliberately have no cross-origin read permission.
fn respond_private(stream: &mut TcpStream, status: &str, ctype: &str, body: &[u8], head: bool) {
    let policy=offline_policy(ctype);
    let header=format!("HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n{policy}Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",status,ctype,body.len());
    let _ = stream.write_all(header.as_bytes());
    if !head {
        let _ = stream.write_all(body);
    }
    let _ = stream.flush();
}

fn respond_private_file(stream: &mut TcpStream, mut file: std::fs::File, ctype: &str, head: bool) {
    let Ok(info) = file.metadata() else {
        respond_private(stream, "404 Not Found", "text/plain", b"not found", head);
        return;
    };
    let header=format!("HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",ctype,info.len());
    if stream.write_all(header.as_bytes()).is_ok() && !head {
        let _ = std::io::copy(&mut file, stream);
    }
    let _ = stream.flush();
}

#[cfg(test)]
mod message_api_tests {
    use super::*;
    use std::io::Read;
    #[test]
    fn groups_cover_all_matches_and_member_pages_use_the_same_filter() {
        let mut document=messages::Doc::new(12);
        for n in 0..120 {document.items.push(messages::Item{id:n.to_string(),ty:"replies".into(),sender_uid:"18446744073709551614".into(),sender_name:"someone".into(),text:if n<60{"needle".into()}else{"other".into()},timestamp:n+1,..Default::default()});}
        document.items.push(messages::Item{id:"unknown".into(),ty:"replies".into(),..Default::default()});
        document.items.push(messages::Item{id:"recent".into(),ty:"replies".into(),sender_uid:"2".into(),timestamp:999,..Default::default()});
        let store=Arc::new(Mutex::new(messages::Store{doc:document,..Default::default()}));let stop=Arc::new(AtomicBool::new(false));
        let port=spawn_with_messages(Arc::new(Mutex::new(ArchiveDoc::default())),store,0,stop.clone()).unwrap();
        let decode=|response:String|->serde_json::Value{assert!(response.starts_with("HTTP/1.1 200"));serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap()};
        let groups=decode(request(port,"/api/messages/groups?type=replies&group_by=user&limit=1","","GET"));
        assert_eq!(groups["total"],3);assert_eq!(groups["total_items"],122);assert_eq!(groups["groups"][0]["count"],1);
        let ranked=decode(request(port,"/api/messages/groups?type=replies&group_by=user&sort=count_desc&limit=1","","GET"));
        assert_eq!(ranked["groups"][0]["count"],120);
        let fewest=decode(request(port,"/api/messages/groups?type=replies&group_by=user&sort=count_asc&offset=2&limit=1","","GET"));
        assert_eq!(fewest["groups"][0]["count"],120);
        assert!(request(port,"/api/messages/groups?type=replies&group_by=user&sort=invalid","","GET").starts_with("HTTP/1.1 400"));
        let members=decode(request(port,"/api/messages/items?type=replies&group_by=user&group_key=user%3A18446744073709551614&offset=100","","GET"));
        assert_eq!(members["total"],120);assert_eq!(members["items"].as_array().unwrap().len(),20);
        let searched=decode(request(port,"/api/messages/items?type=replies&group_by=user&group_key=user%3A18446744073709551614&q=needle&offset=50","","GET"));
        assert_eq!(searched["total"],60);assert_eq!(searched["items"].as_array().unwrap().len(),10);
        assert!(request(port,"/api/messages/groups?type=likes&group_by=user","","GET").starts_with("HTTP/1.1 400"));
        assert!(request(port,"/api/messages/items?type=replies&group_by=user","","GET").starts_with("HTTP/1.1 400"));
        assert!(request(port,"/api/messages/groups?type=replies&group_by=user","https://example.org","GET").starts_with("HTTP/1.1 403"));
        let head=request(port,"/api/messages/groups?type=mentions&group_by=video","","HEAD");assert!(head.ends_with("\r\n\r\n"));assert!(head.contains("Cache-Control: no-store"));
        stop.store(true,Ordering::Relaxed);
    }
    #[test]
    fn subject_metadata_is_archive_bound_same_origin_and_account_scoped() {
        let mut document=messages::Doc::new(12);
        document.items.push(messages::Item{id:"1".into(),ty:"replies".into(),url:"https://www.bilibili.com/video/av170001".into(),raw:json!({"item":{"type":"video","subject_id":170001,"title":"archived title"}}),..Default::default()});
        let store=Arc::new(Mutex::new(messages::Store{doc:document,..Default::default()}));
        let stop=Arc::new(AtomicBool::new(false));
        let port=spawn_with_messages(Arc::new(Mutex::new(ArchiveDoc::default())),store.clone(),0,stop.clone()).unwrap();
        let uri="/api/messages/subjects?account_uid=12&keys=video%3Aaid%3A170001";
        let response=request(port,uri,"","GET");assert!(response.starts_with("HTTP/1.1 200"));assert!(response.contains("Cache-Control: no-store"));
        let value:serde_json::Value=serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(value["subjects"][0]["title"],"archived title");assert_eq!(value["subjects"][0]["bvid"],"BV17x411w7KC");
        assert!(request(port,uri,"https://example.org","GET").starts_with("HTTP/1.1 403"));
        assert!(request(port,"/api/messages/subjects?account_uid=12&keys=video%3Aaid%3A9","","GET").starts_with("HTTP/1.1 400"));
        assert!(request(port,"/api/messages/subjects?account_uid=13&keys=video%3Aaid%3A170001","","GET").starts_with("HTTP/1.1 409"));
        assert!(request(port,uri,"","HEAD").ends_with("\r\n\r\n"));
        assert!(request(port,"/api/messages/subjects?account_uid=12&keys=a,b,c","","GET").starts_with("HTTP/1.1 400"));
        let items=request(port,"/api/messages/items?type=replies","","GET");assert!(items.contains("\"subject\""));assert!(!items.contains("SESSDATA"));
        store.lock().unwrap().doc=messages::Doc::new(13);
        assert!(request(port,uri,"","GET").starts_with("HTTP/1.1 409"));
        stop.store(true,Ordering::Relaxed);
    }
    #[test]
    fn account_identity_is_live_private_and_read_only() {
        let account = Arc::new(Mutex::new(crate::account_ui::PublicAccount {
            avatar_fallback_url: String::new(),
            logged_in: true, mode: "online".into(), library_id: String::new(), uid: "18446744073709551614".into(), name: "测试用户".into(),
            avatar_url: "https://i0.hdslb.com/face.jpg".into(),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let port = spawn_with_account(Arc::new(Mutex::new(ArchiveDoc::default())), Arc::new(Mutex::new(messages::Store::default())), account.clone(), 0, stop.clone()).unwrap();
        let response = request(port,"/api/account","","GET");
        assert!(response.starts_with("HTTP/1.1 200")); assert!(response.contains("Cache-Control: no-store"));
        assert!(!response.contains("Access-Control-Allow-Origin"));
        let value:serde_json::Value=serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(value["uid"],"18446744073709551614");assert_eq!(value.as_object().unwrap().len(),4);
        assert!(request(port,"/api/account","https://example.org","GET").starts_with("HTTP/1.1 403"));
        assert!(request(port,"/api/account","","HEAD").ends_with("\r\n\r\n"));
        assert!(request(port,"/api/account","","POST").starts_with("HTTP/1.1 405"));
        *account.lock().unwrap()=Default::default();
        assert!(request(port,"/api/account","","GET").contains("\"logged_in\":false"));
        stop.store(true, Ordering::Relaxed);
    }
    fn request(port: u16, path: &str, origin: &str, method: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        write!(
            stream,
            "{} {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n{}Connection: close\r\n\r\n",
            method,
            path,
            port,
            if origin.is_empty() {
                String::new()
            } else {
                format!("Origin: {}\r\n", origin)
            }
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }
    #[test]
    fn same_origin_pagination_account_switch_and_head() {
        let state = Arc::new(Mutex::new(messages::Store {
            doc: messages::Doc::new(12),
            ..Default::default()
        }));
        state.lock().unwrap().doc.items.push(messages::Item {
            id: "18446744073709551614".into(),
            ty: "replies".into(),
            text: "hello".into(),
            timestamp: 1,
            ..Default::default()
        });
        state.lock().unwrap().doc.items.push(messages::Item {
            id: "7".into(),
            ty: "likes".into(),
            like_count: 100,
            like_users: vec![messages::LikeUser {
                key: "uid-18446744073709551614".into(),
                uid: "18446744073709551614".into(),
                nickname: "detail user".into(),
                like_time: 10,
                ..Default::default()
            }],
            ..Default::default()
        });
        state.lock().unwrap().doc.sessions.push(messages::Session {
            key: "group_3".into(),
            talker_id: "3".into(),
            session_type: 2,
            ..Default::default()
        });
        state.lock().unwrap().doc.items.push(messages::Item {
            id: "group-message".into(),
            ty: "private".into(),
            session: "group_3".into(),
            ..Default::default()
        });
        let stop = Arc::new(AtomicBool::new(false));
        let port = spawn_with_messages(
            Arc::new(Mutex::new(ArchiveDoc::default())),
            state.clone(),
            0,
            stop.clone(),
        )
        .unwrap();
        struct Shutdown(Arc<AtomicBool>);
        impl Drop for Shutdown {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let _shutdown = Shutdown(stop);
        let response = request(
            port,
            "/api/messages/items?type=replies&q=hello&limit=1",
            "",
            "GET",
        );
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(response.contains("18446744073709551614"));
        assert!(!response.contains("Access-Control-Allow-Origin"));
        let listing = request(
            port,
            "/api/messages/items?type=likes&q=detail%20user",
            "",
            "GET",
        );
        assert!(listing.contains("\"like_users_count\":1"));
        assert!(!listing.contains("\"like_users\":"));
        let details = request(
            port,
            "/api/messages/likes?card_id=7&q=detail%20user&limit=1",
            "",
            "GET",
        );
        assert!(details.starts_with("HTTP/1.1 200"));
        assert!(details.contains("18446744073709551614"));
        assert!(!details.contains("Access-Control-Allow-Origin"));
        let empty = request(port, "/api/messages/likes?card_id=7&offset=1", "", "GET");
        assert!(empty.contains("\"items\":[]"));
        let groups = request(port, "/api/messages/items?type=groups", "", "GET");
        assert!(groups.starts_with("HTTP/1.1 200"));
        assert!(groups.contains("group-message"));
        assert!(groups.contains("\"category\":\"groups\""));
        let combined = request(port, "/api/messages/items?type=private&include_groups=1", "", "GET");
        assert!(combined.contains("group-message"));
        assert!(!combined.contains("\"type\":\"replies\""));
        let private = request(port, "/api/messages/items?type=private", "", "GET");
        assert!(!private.contains("group-message"));
        let counts = request(port, "/api/messages/summary", "", "GET");
        assert!(counts.contains("\"groups\":1"));
        assert!(counts.contains("\"private\":0"));
        let blocked = request(port, "/api/messages/summary", "https://example.org", "GET");
        assert!(blocked.starts_with("HTTP/1.1 403"));
        let head = request(port, "/api/messages/summary", "", "HEAD");
        assert!(head.ends_with("\r\n\r\n"));
        let missing = request(
            port,
            "/api/messages/attachments/..%2fconfig.json",
            "",
            "GET",
        );
        assert!(missing.starts_with("HTTP/1.1 404"));
        state.lock().unwrap().doc = messages::Doc::new(99);
        let response = request(port, "/api/messages/items", "", "GET");
        assert!(!response.contains("18446744073709551614"));
        assert!(response.contains("99"));
    }
    #[test]
    fn profile_route_is_local_readonly_and_account_scoped() {
        let state = Arc::new(Mutex::new(messages::Store { doc: messages::Doc::new(12), ..Default::default() }));
        state.lock().unwrap().doc.sessions.push(messages::Session { key: "user_2".into(), talker_id: "2".into(), session_type: 1, raw: json!({"face":"https://i0.hdslb.com/local.jpg"}), ..Default::default() });
        state.lock().unwrap().doc.sessions.push(messages::Session { key: "user_3".into(), talker_id: "3".into(), session_type: 1, ..Default::default() });
        state.lock().unwrap().doc.items.push(messages::Item { id: "private-3".into(), ty: "private".into(), session: "user_3".into(), sender_uid: "3".into(), ..Default::default() });
        state.lock().unwrap().doc.items.push(messages::Item { id: "reply-3".into(), ty: "replies".into(), sender_uid: "3".into(), raw: json!({"user":{"mid":"3","avatar":"https://i0.hdslb.com/reused.jpg"}}), ..Default::default() });
        let before = serde_json::to_string(&state.lock().unwrap().doc).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let port = spawn_with_messages(Arc::new(Mutex::new(ArchiveDoc::default())), state.clone(), 0, stop.clone()).unwrap();
        struct Shutdown(Arc<AtomicBool>); impl Drop for Shutdown { fn drop(&mut self) { self.0.store(true, Ordering::SeqCst); } }
        let _stop = Shutdown(stop);
        let summary = request(port, "/api/messages/summary", "", "GET"); assert!(summary.contains("reused.jpg"));
        let page = request(port, "/api/messages/items?type=private&session=user_3", "", "GET"); assert!(page.contains("\"sender_avatar_url\":\"https://i0.hdslb.com/reused.jpg\""));
        let route = "/api/messages/profile?account_uid=12&uid=2";
        let found = request(port, route, "", "GET");
        assert!(found.starts_with("HTTP/1.1 200")); assert!(found.contains("local.jpg"));
        assert!(!found.contains("SESSDATA")); assert!(found.contains("Cache-Control: no-store"));
        assert!(request(port, route, "https://example.org", "GET").starts_with("HTTP/1.1 403"));
        assert!(request(port, route, "", "HEAD").ends_with("\r\n\r\n"));
        assert!(request(port, "/api/messages/profile?account_uid=12&uid=99", "", "GET").starts_with("HTTP/1.1 400"));
        assert!(request(port, "/api/messages/profile?account_uid=99&uid=2", "", "GET").starts_with("HTTP/1.1 409"));
        assert_eq!(before, serde_json::to_string(&state.lock().unwrap().doc).unwrap());
        state.lock().unwrap().doc = messages::Doc::new(99);
        assert!(request(port, route, "", "GET").starts_with("HTTP/1.1 409"));
    }

}
