//! Platform credential protection. Never falls back to plaintext.
use anyhow::{anyhow, bail, Context, Result};
/// DPAPI 里的 DATA_BLOB 结构
#[cfg(windows)]
#[repr(C)]
struct DataBlob {
    cb_data: u32,
    pb_data: *mut u8,
}

#[cfg(windows)]
#[link(name = "crypt32")]
extern "system" {
    fn CryptProtectData(
        p_data_in: *const DataBlob,
        sz_data_descr: *const u16,
        p_optional_entropy: *const DataBlob,
        pv_reserved: *mut std::ffi::c_void,
        p_prompt_struct: *mut std::ffi::c_void,
        dw_flags: u32,
        p_data_out: *mut DataBlob,
    ) -> i32;
    fn CryptUnprotectData(
        p_data_in: *const DataBlob,
        sz_data_descr: *mut *mut u16,
        p_optional_entropy: *const DataBlob,
        pv_reserved: *mut std::ffi::c_void,
        p_prompt_struct: *mut std::ffi::c_void,
        dw_flags: u32,
        p_data_out: *mut DataBlob,
    ) -> i32;
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn LocalFree(h_mem: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
}

const PREFIX: &str = "enc:";
#[cfg(windows)]
const UI_FORBIDDEN: u32 = 0x1; // 不弹任何 DPAPI 界面

/// Protect a value; encryption failure must abort persistence.
pub fn seal(plain: &str) -> Result<String> {
    if plain.is_empty() {
        return Ok(String::new());
    }
    #[cfg(windows)]
    {
        let input = DataBlob {
            cb_data: u32::try_from(plain.len())?,
            pb_data: plain.as_ptr() as *mut u8,
        };
        let mut output = DataBlob {
            cb_data: 0,
            pb_data: std::ptr::null_mut(),
        };
        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok != 0 && !output.pb_data.is_null() {
            let bytes =
                unsafe { std::slice::from_raw_parts(output.pb_data, output.cb_data as usize) };
            let hex = to_hex(bytes);
            unsafe {
                LocalFree(output.pb_data as *mut std::ffi::c_void);
            }
            return Ok(format!("{}{}", PREFIX, hex));
        }
    }
    bail!("平台加密不可用，登录信息未保存")
}

/// Read legacy plaintext or DPAPI ciphertext; invalid ciphertext is an error.
pub fn open(stored: &str) -> Result<String> {
    if let Some(hex) = stored.strip_prefix(PREFIX) {
        #[cfg(windows)]
        {
            if let Some(bytes) = from_hex(hex) {
                let input = DataBlob {
                    cb_data: u32::try_from(bytes.len())?,
                    pb_data: bytes.as_ptr() as *mut u8,
                };
                let mut output = DataBlob {
                    cb_data: 0,
                    pb_data: std::ptr::null_mut(),
                };
                let ok = unsafe {
                    CryptUnprotectData(
                        &input,
                        std::ptr::null_mut(),
                        std::ptr::null(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        UI_FORBIDDEN,
                        &mut output,
                    )
                };
                if ok != 0 && !output.pb_data.is_null() {
                    let decrypted = unsafe {
                        std::slice::from_raw_parts(output.pb_data, output.cb_data as usize)
                    };
                    let text = String::from_utf8(decrypted.to_vec());
                    unsafe {
                        LocalFree(output.pb_data as *mut std::ffi::c_void);
                    }
                    return text.map_err(|_| anyhow!("凭证编码无效，请重新登录"));
                }
            }
        }
        bail!("凭证无法解密，请重新登录");
    }
    if stored.is_empty() { return Ok(String::new()); }
    bail!("不再支持明文凭证，请重新登录")
}

#[cfg(any(windows, test))]
fn to_hex(bytes: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(H[(b >> 4) as usize] as char);
        out.push(H[(b & 0xf) as usize] as char);
    }
    out
}

#[cfg(any(windows, test))]
fn from_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    let val = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    let mut i = 0;
    while i < bytes.len() {
        out.push((val(bytes[i])? << 4) | val(bytes[i + 1])?);
        i += 2;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        let raw = b"hello\x00\xff";
        assert_eq!(from_hex(&to_hex(raw)), Some(raw.to_vec()));
        assert_eq!(from_hex("abc"), None);
        assert_eq!(from_hex("zz"), None);
    }

    #[test]
    fn plaintext_is_rejected() {
        assert!(open("plain-value").is_err());
        assert_eq!(seal("").unwrap(), "");
    }
}

#[derive(Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Secrets {
    pub sessdata: String,
    pub buvid3: String,
    pub buvid4: String,
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secrets([REDACTED])")
    }
}

const FIELDS: [&str; 3] = ["sessdata", "buvid3", "buvid4"];
const STORE_VERSION: &str = "secure-store:v1";

pub fn preserve(old: &serde_json::Value, new: &mut serde_json::Value) {
    for key in FIELDS.into_iter().chain(["credential_storage"]) {
        if let Some(value) = old.get(key) {
            new[key] = value.clone();
        }
    }
}

pub fn load(v: &serde_json::Value) -> Result<Secrets> {
    if let Some(version) = v.get("credential_storage") {
        if version.as_str() != Some(STORE_VERSION) {
            bail!("不支持的凭据存储版本");
        }
        return platform_load();
    }
    Ok(Secrets {
        sessdata: open(v["sessdata"].as_str().unwrap_or_default())?,
        buvid3: open(v["buvid3"].as_str().unwrap_or_default())?,
        buvid4: open(v["buvid4"].as_str().unwrap_or_default())?,
    })
}

pub fn store(secrets: &Secrets, v: &mut serde_json::Value) -> Result<()> {
    // Work on a copy so failures cannot leave a partially updated configuration.
    let mut next = v.clone();
    platform_store(secrets, &mut next)?;
    *v = next;
    Ok(())
}

#[cfg(windows)]
fn platform_store(secrets: &Secrets, v: &mut serde_json::Value) -> Result<()> {
    for (key, plain) in
        FIELDS
            .into_iter()
            .zip([&secrets.sessdata, &secrets.buvid3, &secrets.buvid4])
    {
        let encrypted = seal(plain)?;
        if open(&encrypted)? != *plain {
            bail!("凭证加密校验失败，登录信息未保存");
        }
        v[key] = encrypted.into();
    }
    v.as_object_mut()
        .ok_or_else(|| anyhow!("配置格式无效"))?
        .remove("credential_storage");
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn entry() -> Result<keyring::v1::Entry> {
    keyring::v1::Entry::new("BiliCommentHub", "credentials-v1")
        .map_err(|_| anyhow!("系统凭据库不可用，登录信息未保存"))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn platform_load() -> Result<Secrets> {
    let text = entry()?
        .get_password()
        .map_err(|_| anyhow!("无法读取系统凭证，请解锁凭据库或重新登录"))?;
    serde_json::from_str(&text).map_err(|_| anyhow!("系统凭证格式无效，请重新登录"))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_load() -> Result<Secrets> {
    bail!("此平台无法读取系统凭据库，请重新登录")
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn platform_store(secrets: &Secrets, v: &mut serde_json::Value) -> Result<()> {
    store_vault(secrets, v, &entry()?)
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
trait Vault {
    fn read(&self) -> Result<String>;
    fn write(&self, text: &str) -> Result<()>;
    fn delete(&self) -> Result<()>;
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Vault for keyring::v1::Entry {
    fn read(&self) -> Result<String> {
        self.get_password()
            .map_err(|_| anyhow!("系统凭据库读取失败"))
    }
    fn write(&self, text: &str) -> Result<()> {
        self.set_password(text)
            .map_err(|_| anyhow!("系统凭据库写入失败，登录信息未保存"))
    }
    fn delete(&self) -> Result<()> {
        match self.delete_credential() {
            Ok(()) | Err(keyring::v1::Error::NoEntry) => Ok(()),
            Err(_) => bail!("系统凭证删除失败，请解锁凭据库后重试"),
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
fn store_vault(secrets: &Secrets, v: &mut serde_json::Value, vault: &impl Vault) -> Result<()> {
    let mut next = v.clone();
    let object = next
        .as_object_mut()
        .ok_or_else(|| anyhow!("配置格式无效"))?;
    if secrets == &Secrets::default() {
        vault.delete()?;
        object.remove("credential_storage");
    } else {
        let text = serde_json::to_string(secrets)?;
        vault.write(&text)?;
        if vault.read()? != text {
            bail!("系统凭据库校验失败，登录信息未保存");
        }
        object.insert("credential_storage".into(), STORE_VERSION.into());
    }
    for key in FIELDS {
        object.remove(key);
    }
    *v = next;
    Ok(())
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn platform_store(_: &Secrets, _: &mut serde_json::Value) -> Result<()> {
    bail!("此平台不支持安全存储，登录信息未保存")
}

/// Write in the same directory, then atomically replace the original file.
pub fn write_config(path: &std::path::Path, v: &serde_json::Value) -> Result<()> {
    use std::io::Write;
    let dir = path.parent().ok_or_else(|| anyhow!("配置目录无效"))?;
    std::fs::create_dir_all(dir).map_err(|_| anyhow!("无法创建配置目录"))?;
    let mut file =
        tempfile::NamedTempFile::new_in(dir).map_err(|_| anyhow!("无法创建配置临时文件"))?;
    let text = serde_json::to_vec_pretty(v)?;
    file.write_all(&text)
        .and_then(|_| file.as_file().sync_all())
        .map_err(|_| anyhow!("配置写入失败，原文件已保留"))?;
    file.persist(path)
        .map_err(|error| error.error)
        .context("配置替换失败，原文件已保留")?;
    Ok(())
}

#[cfg(test)]
mod storage_tests {
    use super::*;

    #[test]
    fn invalid_ciphertext_is_never_a_cookie() {
        for value in ["enc:", "enc:zz", "enc:123", "enc:00000000"] {
            assert!(open(value).is_err());
            assert!(load(&serde_json::json!({"sessdata": value})).is_err());
        }
        assert!(load(&serde_json::json!({"credential_storage": "future"})).is_err());
    }

    #[test]
    fn preserve_unreadable_credentials_and_change_settings() {
        let old = serde_json::json!({"sessdata":"enc:broken", "credential_storage": "future", "web_port": 8686});
        let mut next = serde_json::json!({"web_port": 9000});
        preserve(&old, &mut next);
        assert_eq!(next["sessdata"], old["sessdata"]);
        assert_eq!(next["credential_storage"], old["credential_storage"]);
        assert_eq!(next["web_port"], 9000);
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_roundtrip_and_clear() {
        let old = serde_json::json!({"sessdata":"test-cookie-测试", "buvid3":"device-3", "buvid4":"device-4"});
        let secrets = Secrets { sessdata: "test-cookie-测试".into(), buvid3: "device-3".into(), buvid4: "device-4".into() };
        let mut encrypted = serde_json::json!({"web_port":8686});
        store(&secrets, &mut encrypted).unwrap();
        assert_eq!(load(&encrypted).unwrap(), secrets);
        assert!(encrypted["sessdata"].as_str().unwrap().starts_with("enc:"));
        assert!(!serde_json::to_string(&encrypted)
            .unwrap()
            .contains("test-cookie"));
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.json");
        write_config(&path, &old).unwrap();
        write_config(&path, &encrypted).unwrap();
        let saved = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(load(&saved).unwrap(), secrets);
        store(&Secrets::default(), &mut encrypted).unwrap();
        assert_eq!(load(&encrypted).unwrap(), Secrets::default());
    }

    #[test]
    fn failed_replace_leaves_existing_target_intact() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.json");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("keep"), b"unchanged").unwrap();
        assert!(write_config(&path, &serde_json::json!({})).is_err());
        assert_eq!(std::fs::read(path.join("keep")).unwrap(), b"unchanged");
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
    }
}

#[cfg(test)]
mod vault_tests {
    use super::*;
    use std::cell::RefCell;
    struct FakeVault {
        value: RefCell<String>,
        fail: bool,
        corrupt: bool,
    }
    impl Vault for FakeVault {
        fn read(&self) -> Result<String> {
            if self.corrupt {
                return Ok("corrupt".into());
            }
            Ok(self.value.borrow().clone())
        }
        fn write(&self, text: &str) -> Result<()> {
            if self.fail {
                bail!("locked");
            }
            *self.value.borrow_mut() = text.into();
            Ok(())
        }
        fn delete(&self) -> Result<()> {
            if self.fail {
                bail!("locked");
            }
            self.value.borrow_mut().clear();
            Ok(())
        }
    }
    #[test]
    fn vault_roundtrip_and_delete() {
        let vault = FakeVault {
            value: RefCell::new(String::new()),
            fail: false,
            corrupt: false,
        };
        let secrets = Secrets {
            sessdata: "fake-cookie".into(),
            ..Secrets::default()
        };
        let mut config = serde_json::json!({"sessdata":"legacy", "web_port":9000});
        store_vault(&secrets, &mut config, &vault).unwrap();
        assert_eq!(
            serde_json::from_str::<Secrets>(&vault.read().unwrap()).unwrap(),
            secrets
        );
        assert!(config.get("sessdata").is_none());
        assert_eq!(config["credential_storage"], STORE_VERSION);
        store_vault(&Secrets::default(), &mut config, &vault).unwrap();
        assert!(config.get("credential_storage").is_none());
        assert_eq!(vault.read().unwrap(), "");
        assert_eq!(config["web_port"], 9000);
    }
    #[test]
    fn locked_or_unverified_vault_preserves_config() {
        for (fail, corrupt) in [(true, false), (false, true)] {
            let vault = FakeVault {
                value: RefCell::new(String::new()),
                fail,
                corrupt,
            };
            let original = serde_json::json!({"sessdata":"legacy"});
            let mut config = original.clone();
            let secrets = Secrets {
                sessdata: "new-cookie".into(),
                ..Secrets::default()
            };
            assert!(store_vault(&secrets, &mut config, &vault).is_err());
            assert_eq!(config, original);
            if fail {
                assert!(store_vault(&Secrets::default(), &mut config, &vault).is_err());
                assert_eq!(config, original);
            }
        }
    }
}
