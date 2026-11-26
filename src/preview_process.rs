use crate::{account_ui::PublicAccount, message_profiles, message_subjects, Shared};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex, Weak,
    },
    time::Duration,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub root: PathBuf,
    pub uid: u64,
    pub offline: bool,
    pub precache: bool,
    pub account: PublicAccount,
    pub standalone: Option<PathBuf>,
}

impl Scope {
    fn current(shared: &Shared) -> Self {
        let cfg = shared.cfg.lock().unwrap();
        let active = crate::library::active();
        let offline = shared.offline.load(Ordering::SeqCst);
        let uid = shared.account_uid.load(Ordering::SeqCst);
        let account = shared.public_account.lock().unwrap().clone();
        let coherent = if let Some(active) = &active {
            offline
                && active.offline
                && account.mode == "offline"
                && account.library_id == active.id
                && account.uid == uid.to_string()
        } else {
            !offline && (uid == 0 || (account.logged_in && account.uid == uid.to_string()))
        };
        Self {
            root: if coherent {
                crate::export_dir_of(&cfg).into()
            } else {
                PathBuf::new()
            },
            uid: if coherent { uid } else { 0 },
            offline,
            precache: cfg.archive_precache,
            account: if coherent {
                account
            } else {
                PublicAccount::default()
            },
            standalone: (coherent && uid == 0 && !offline)
                .then(crate::preload_path)
                .flatten(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum Lookup {
    Profile(message_profiles::Profile),
    Subject(message_subjects::Subject),
    PublicSubject(message_subjects::Subject),
}

#[derive(Serialize, Deserialize)]
enum Input {
    Start { port: u16, scope: Scope },
    Scope(Scope),
    Answer { id: u64, value: Lookup },
}

#[derive(Serialize, Deserialize)]
enum Output {
    Ready(Result<u16, String>),
    Lookup {
        id: u64,
        account_uid: u64,
        library_id: String,
        retry: bool,
        value: Lookup,
    },
}

fn send<T: Serialize>(writer: &mut impl Write, value: &T) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

fn receive<T: for<'a> Deserialize<'a>>(reader: &mut impl BufRead) -> Result<T> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Err(anyhow!("预览进程通信已关闭"));
    }
    Ok(serde_json::from_str(&line)?)
}

pub struct Process {
    child: Child,
    stopping: Arc<AtomicBool>,
}

impl Process {
    pub fn start(
        shared: &Arc<Shared>,
        port: u16,
        exited: impl FnOnce() + Send + 'static,
    ) -> Result<(u16, Self)> {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("--preview-worker")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        let child = command.spawn().context("无法启动网页预览进程")?;
        let mut process = Self {
            child,
            stopping: Arc::new(AtomicBool::new(false)),
        };
        let stdin = Arc::new(Mutex::new(
            process.child.stdin.take().context("预览进程缺少输入管道")?,
        ));
        let mut reader = BufReader::new(
            process
                .child
                .stdout
                .take()
                .context("预览进程缺少输出管道")?,
        );
        let scope = Scope::current(shared);
        send(
            &mut *stdin.lock().unwrap(),
            &Input::Start {
                port,
                scope: scope.clone(),
            },
        )?;
        let (ready, result) = mpsc::sync_channel(1);
        let stopping = process.stopping.clone();
        let owner = Arc::downgrade(shared);
        let answers = stdin.clone();
        std::thread::spawn(move || {
            match receive(&mut reader) {
                Ok(Output::Ready(Ok(port))) => {
                    let _ = ready.send(Ok(port));
                }
                Ok(Output::Ready(Err(error))) => {
                    let _ = ready.send(Err(error));
                    return;
                }
                _ => {
                    let _ = ready.send(Err("预览进程启动时退出".into()));
                    return;
                }
            }
            let profiles = Arc::new(message_profiles::Service::default());
            let subjects = Arc::new(message_subjects::Service::default());
            while let Ok(Output::Lookup {
                id,
                account_uid,
                library_id,
                retry,
                value,
            }) = receive(&mut reader)
            {
                let (owner, profiles, subjects, answers) = (
                    owner.clone(),
                    profiles.clone(),
                    subjects.clone(),
                    answers.clone(),
                );
                std::thread::spawn(move || {
                    let value = enrich(
                        owner,
                        account_uid,
                        library_id,
                        retry,
                        value,
                        &profiles,
                        &subjects,
                    );
                    let _ = send(&mut *answers.lock().unwrap(), &Input::Answer { id, value });
                });
            }
            if !stopping.swap(true, Ordering::AcqRel) {
                exited();
            }
        });
        let bound = result
            .recv_timeout(Duration::from_secs(5))
            .context("等待网页预览进程启动超时")?
            .map_err(|e| anyhow!("{e}"))?;
        let owner = Arc::downgrade(shared);
        let stopping = process.stopping.clone();
        std::thread::spawn(move || {
            let mut previous = scope;
            while !stopping.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(200));
                let Some(shared) = owner.upgrade() else {
                    break;
                };
                let next = Scope::current(&shared);
                if next != previous {
                    if send(&mut *stdin.lock().unwrap(), &Input::Scope(next.clone())).is_err() {
                        break;
                    }
                    previous = next;
                }
            }
        });
        Ok((bound, process))
    }

    pub fn running(&self) -> bool {
        !self.stopping.load(Ordering::Acquire)
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn enrich(
    owner: Weak<Shared>,
    account_uid: u64,
    library_id: String,
    retry: bool,
    value: Lookup,
    profiles: &message_profiles::Service,
    subjects: &message_subjects::Service,
) -> Lookup {
    if let Lookup::PublicSubject(subject) = &value {
        let current = || {
            owner.upgrade().is_some_and(|shared| {
                let scope = Scope::current(&shared);
                scope.offline && scope.uid == account_uid && scope.account.library_id == library_id
            })
        };
        if current() && matches!(subject.ty.as_str(), "video" | "dynamic") {
            let result = subjects.lookup_public(
                &library_id,
                &account_uid.to_string(),
                subject.clone(),
                retry,
                current,
            );
            return Lookup::PublicSubject(result);
        }
        return value;
    }
    let matches_scope = |scope: &Scope| {
        !scope.offline
            && scope.uid == account_uid
            && scope.account.library_id == library_id
            && scope.account.logged_in
    };
    if !owner
        .upgrade()
        .is_some_and(|shared| matches_scope(&Scope::current(&shared)))
    {
        return value;
    }
    let scope_id = library_id.clone();
    let weak = owner.clone();
    let credentials: message_subjects::Credentials = Arc::new(move |uid| {
        let shared = weak.upgrade()?;
        let scope = Scope::current(&shared);
        if scope.offline
            || scope.uid != account_uid
            || uid != account_uid.to_string()
            || scope.account.library_id != scope_id
            || !scope.account.logged_in
        {
            return None;
        }
        let cfg = shared.cfg.lock().unwrap();
        (!cfg.sessdata.is_empty()).then(|| cfg.sessdata.clone())
    });
    let pacing: message_profiles::Pacing = Arc::new(move || {
        owner.upgrade().map_or((1800, 2400), |shared| {
            let cfg = shared.cfg.lock().unwrap();
            (cfg.interval_ms as u64, cfg.interval_max_ms as u64)
        })
    });
    let uid = account_uid.to_string();
    match value {
        Lookup::Profile(profile) => {
            Lookup::Profile(profiles.lookup(&uid, profile, retry, &credentials, &pacing))
        }
        Lookup::Subject(subject) => {
            Lookup::Subject(subjects.lookup(&uid, subject, retry, &credentials))
        }
        Lookup::PublicSubject(subject) => Lookup::PublicSubject(subject),
    }
}

type Pending = Arc<Mutex<HashMap<u64, mpsc::SyncSender<Lookup>>>>;

pub struct Broker {
    next_id: AtomicU64,
    requests: Pending,
    writer: Mutex<std::io::Stdout>,
}

impl Broker {
    pub fn lookup(&self, scope: &Scope, value: Lookup, retry: bool) -> Lookup {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::sync_channel(1);
        {
            let mut requests = self.requests.lock().unwrap();
            if scope.offline != matches!(&value, Lookup::PublicSubject(_))
                || scope.uid == 0
                || requests.len() >= 6
            {
                return value;
            }
            requests.insert(id, tx);
        }
        let written = send(
            &mut *self.writer.lock().unwrap(),
            &Output::Lookup {
                id,
                account_uid: scope.uid,
                library_id: scope.account.library_id.clone(),
                retry,
                value: value.clone(),
            },
        );
        let answer = written
            .ok()
            .and_then(|_| rx.recv_timeout(Duration::from_secs(30)).ok());
        self.requests.lock().unwrap().remove(&id);
        answer.unwrap_or(value)
    }
}

pub fn run() -> i32 {
    let mut reader = BufReader::new(std::io::stdin());
    let Ok(Input::Start { port, scope }) = receive(&mut reader) else {
        return 2;
    };
    let disk = Arc::new(crate::preview_disk::Disk::new(scope));
    let requests: Pending = Arc::default();
    let broker = Arc::new(Broker {
        next_id: AtomicU64::new(1),
        requests: requests.clone(),
        writer: Mutex::new(std::io::stdout()),
    });
    let listener = crate::web::spawn_disk(port, disk.clone(), broker.clone());
    let failed = listener.is_err();
    if send(
        &mut *broker.writer.lock().unwrap(),
        &Output::Ready(listener),
    )
    .is_err()
        || failed
    {
        return 1;
    }
    disk.warm();
    while let Ok(input) = receive(&mut reader) {
        match input {
            Input::Start { .. } => return 2,
            Input::Scope(scope) => {
                disk.set_scope(scope);
                disk.warm();
            }
            Input::Answer { id, value } => {
                if let Some(waiter) = requests.lock().unwrap().remove(&id) {
                    let _ = waiter.send(value);
                }
            }
        }
    }
    0
}
