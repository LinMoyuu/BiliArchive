//! The account header is independent of generic task and web-server status.
use crate::{bili, checkpoint, emit, push_log, AppWindow, Shared};
use md5::{Digest, Md5};
use slint::Weak;
use std::{
    io::Read,
    path::PathBuf,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

/// Only public identity is shared with the local web server; never credentials.
#[derive(Clone, Default, serde::Serialize)]
pub struct PublicAccount {
    pub logged_in: bool,
    pub mode: String,
    pub library_id: String,
    pub uid: String,
    pub name: String,
    pub avatar_url: String,
    pub avatar_fallback_url: String,
}

fn avatar_url(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let value = if let Some(rest) = value.strip_prefix("http://") {
        format!("https://{rest}")
    } else {
        value.to_owned()
    };
    let url = reqwest::Url::parse(&value).ok()?;
    (url.scheme() == "https" && url.host_str().is_some()).then(|| url.to_string())
}
pub fn public_image_url(value: &str) -> Option<String> {
    let value = value.trim();
    let candidate = if value.starts_with("//") {
        format!("https:{value}")
    } else {
        avatar_url(value)?
    };
    let url = reqwest::Url::parse(&candidate).ok()?;
    let host = url.host_str()?;
    (url.scheme() == "https"
        && (host == "hdslb.com" || host.ends_with(".hdslb.com"))
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none())
    .then(|| url.to_string())
}

fn archive_avatar_is_current(
    shared: &Shared,
    library_id: &str,
    uid: u64,
    generation: usize,
) -> bool {
    shared.offline.load(Ordering::SeqCst)
        && shared.qr_gen.load(Ordering::SeqCst) == generation
        && shared.account_uid.load(Ordering::SeqCst) == uid
        && crate::library::active().is_some_and(|active| active.offline && active.id == library_id)
}

pub fn show_archive_avatar(
    shared: &Arc<Shared>,
    ui: &Weak<AppWindow>,
    library_id: String,
    uid: u64,
    local: Option<PathBuf>,
    remote: String,
) {
    let shared = shared.clone();
    let weak = ui.clone();
    let generation = shared.qr_gen.load(Ordering::SeqCst);
    emit(ui, move |window| {
        if !archive_avatar_is_current(&shared, &library_id, uid, generation) {
            return;
        }
        if let Some(image) = local.and_then(|path| slint::Image::load_from_path(&path).ok()) {
            window.set_account_avatar(image);
            window.set_account_has_avatar(true);
            return;
        }
        let Some(url) = public_image_url(&remote) else {
            return;
        };
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<tempfile::NamedTempFile> {
                let client = reqwest::blocking::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(Duration::from_secs(10))
                    .build()?;
                let response = client.get(url).send()?.error_for_status()?;
                anyhow::ensure!(response.status().is_success(), "公开头像地址已变更");
                let mut file = tempfile::Builder::new().suffix(".image").tempfile()?;
                let length = std::io::copy(&mut response.take(4 * 1024 * 1024 + 1), &mut file)?;
                anyhow::ensure!(length > 0 && length <= 4 * 1024 * 1024, "头像文件大小无效");
                Ok(file)
            })();
            if let Ok(file) = result {
                emit(&weak, move |window| {
                    if !archive_avatar_is_current(&shared, &library_id, uid, generation) {
                        return;
                    }
                    if let Ok(image) = slint::Image::load_from_path(file.path()) {
                        window.set_account_avatar(image);
                        window.set_account_has_avatar(true);
                    }
                });
            }
        });
    });
}

fn cache_path(uid: u64, url: &str) -> anyhow::Result<PathBuf> {
    let hash = format!("{:x}", Md5::digest(url.as_bytes()));
    Ok(checkpoint::data_dir()?
        .join("cache")
        .join("avatars")
        .join(format!("{uid}-{hash}.image")))
}
fn download_avatar(uid: u64, url: &str) -> anyhow::Result<PathBuf> {
    let path = cache_path(uid, url)?;
    if path.is_file() {
        return Ok(path);
    }
    let response = bili::http_client()?
        .get(url)
        .header("Referer", "https://www.bilibili.com/")
        .timeout(Duration::from_secs(10))
        .send()?
        .error_for_status()?;
    let mut bytes = Vec::new();
    response.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        !bytes.is_empty() && bytes.len() <= 4 * 1024 * 1024,
        "头像内容为空或过大"
    );
    std::fs::create_dir_all(path.parent().unwrap())?;
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    std::io::Write::write_all(&mut file, &bytes)?;
    file.persist(&path)?;
    Ok(path)
}
fn current(shared: &Shared, uid: u64, cookie: &str) -> bool {
    !shared.offline.load(Ordering::SeqCst)
        && shared.account_uid.load(Ordering::SeqCst) == uid
        && shared.cfg.lock().unwrap().sessdata == cookie
}
/// Account changes invalidate old avatar responses before applying the next profile.
pub fn reset(shared: &Arc<Shared>, ui: &Weak<AppWindow>, uid: u64) {
    if shared.account_uid.load(Ordering::SeqCst) != uid {
        return;
    }
    *shared.public_account.lock().unwrap() = PublicAccount::default();
    let shared = shared.clone();
    emit(ui, move |w| {
        if shared.account_uid.load(Ordering::SeqCst) != uid {
            return;
        }
        w.set_account_logged_in(false);
        w.set_account_name("".into());
        w.set_account_uid("".into());
        w.set_account_text("".into());
        w.set_account_avatar_key("".into());
        w.set_account_has_avatar(false);
        w.set_account_avatar(slint::Image::default());
    });
}
pub fn publish(
    shared: &Arc<Shared>,
    ui: &Weak<AppWindow>,
    profile: bili::LoginProfile,
    cookie: String,
) {
    let uid = profile.uid;
    let avatar = avatar_url(&profile.avatar).unwrap_or_default();
    let name = if profile.name.is_empty() {
        uid.to_string()
    } else {
        profile.name
    };
    {
        let cfg = shared.cfg.lock().unwrap();
        if shared.offline.load(Ordering::SeqCst)
            || shared.account_uid.load(Ordering::SeqCst) != uid
            || cfg.sessdata != cookie
        {
            return;
        }
        *shared.public_account.lock().unwrap() = PublicAccount {
            logged_in: true,
            mode: "online".into(),
            library_id: crate::library::current_id().unwrap_or_default(),
            uid: uid.to_string(),
            name: name.clone(),
            avatar_url: avatar.clone(),
            avatar_fallback_url: String::new(),
        };
    }
    let account = shared.clone();
    let expected = cookie.clone();
    let key = avatar.clone();
    emit(ui, move |w| {
        if !current(&account, uid, &expected) {
            return;
        }
        let same_avatar = w.get_account_uid().as_str() == uid.to_string()
            && w.get_account_avatar_key().as_str() == key;
        w.set_account_logged_in(true);
        w.set_account_uid(uid.to_string().into());
        w.set_account_name(name.clone().into());
        w.set_account_text(format!("已登录：{name}（UID {uid}）").into());
        w.set_account_avatar_key(key.into());
        if !same_avatar {
            w.set_account_has_avatar(false);
            w.set_account_avatar(slint::Image::default());
        }
    });
    if avatar.is_empty() {
        return;
    }
    let shared = shared.clone();
    let ui = ui.clone();
    std::thread::spawn(move || {
        if !current(&shared, uid, &cookie) {
            return;
        }
        match download_avatar(uid, &avatar) {
            Ok(path) => {
                let account = shared.clone();
                emit(&ui, move |w| {
                    if !current(&account, uid, &cookie)
                        || w.get_account_avatar_key().as_str() != avatar
                    {
                        return;
                    }
                    if let Ok(image) = slint::Image::load_from_path(&path) {
                        w.set_account_avatar(image);
                        w.set_account_has_avatar(true);
                    }
                });
            }
            Err(_) => {
                if current(&shared, uid, &cookie) {
                    push_log(&shared, &ui, "头像载入失败，使用默认头像");
                }
            }
        }
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn avatar_urls_are_optional_and_use_https() {
        assert_eq!(
            avatar_url("http://i0.hdslb.com/face.png").as_deref(),
            Some("https://i0.hdslb.com/face.png")
        );
        for url in [
            "",
            "file:///private",
            "javascript:alert(1)",
            "data:image/png;base64,a",
            "not a url",
        ] {
            assert!(avatar_url(url).is_none());
        }
    }
}
