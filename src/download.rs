use crate::config::Quality;
use crate::gpu::Gpu;
use crate::i18n::t;
use crate::library::{self, Asset};
use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// sylvan.apple.com chains to "Apple Root CA", which the usual trust stores lack.
const APPLE_ROOT_CA: &[u8] = include_bytes!("../data/apple-root-ca.cer");

pub struct Params {
    pub lib: PathBuf,
    pub quality: Quality,
    pub refresh_catalog: bool,
    pub jobs: usize,
    pub render: Option<Gpu>,
}

#[derive(Debug)]
pub enum Msg {
    Phase(String),
    Planned {
        todo: usize,
        catalog: usize,
    },
    ClipDone {
        name: String,
        error: Option<String>,
    },
    Finished {
        cancelled: bool,
        error: Option<String>,
    },
}

pub struct Handle {
    pub cancel: Arc<AtomicBool>,
    pub bytes: Arc<AtomicU64>,
    pub rx: async_channel::Receiver<Msg>,
}

#[derive(Debug)]
enum Error {
    Cancelled,
    Other(String),
}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error::Other(s)
    }
}

fn agent() -> ureq::Agent {
    let root = ureq::tls::Certificate::from_der(APPLE_ROOT_CA);
    let tls = ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::new_with_certs(&[root]))
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .http_status_as_error(false)
        .user_agent(concat!("Aloft/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

pub fn start(p: Params) -> Handle {
    let (tx, rx) = async_channel::unbounded();
    let cancel = Arc::new(AtomicBool::new(false));
    let bytes = Arc::new(AtomicU64::new(0));
    let h = Handle {
        cancel: cancel.clone(),
        bytes: bytes.clone(),
        rx,
    };
    std::thread::spawn(move || {
        let res = run(&p, &tx, &cancel, &bytes);
        let msg = match res {
            Ok(()) => Msg::Finished {
                cancelled: false,
                error: None,
            },
            Err(Error::Cancelled) => Msg::Finished {
                cancelled: true,
                error: None,
            },
            Err(Error::Other(e)) => Msg::Finished {
                cancelled: false,
                error: Some(e),
            },
        };
        let _ = tx.send_blocking(msg);
    });
    h
}

fn fetch_catalog(agent: &ureq::Agent, dest: &Path) -> Result<(), String> {
    let resp = agent.get(library::CATALOG_URL).call().map_err(|e| {
        format!(
            "{}: {e}",
            t("Catalog download failed", "Katalog-Download fehlgeschlagen")
        )
    })?;
    if resp.status() != 200 {
        return Err(format!("Catalog: HTTP {}", resp.status()));
    }
    let data = resp
        .into_body()
        .with_config()
        .limit(512 << 20)
        .read_to_vec()
        .map_err(|e| e.to_string())?;
    let mut archive = tar::Archive::new(&data[..]);
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let is_catalog = entry
            .path()
            .ok()
            .is_some_and(|p| p.file_name().is_some_and(|n| n == "entries.json"));
        if is_catalog {
            let mut json = Vec::new();
            entry.read_to_end(&mut json).map_err(|e| e.to_string())?;
            library::parse_catalog(&json)?;
            let tmp = dest.with_extension("json.tmp");
            std::fs::write(&tmp, &json).map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, dest).map_err(|e| e.to_string())?;
            return Ok(());
        }
    }
    Err("entries.json missing in catalog archive".into())
}

struct Job {
    asset: Asset,
    target: PathBuf,
}

fn run(
    p: &Params,
    tx: &async_channel::Sender<Msg>,
    cancel: &Arc<AtomicBool>,
    bytes: &Arc<AtomicU64>,
) -> Result<(), Error> {
    let agent = agent();
    std::fs::create_dir_all(&p.lib).map_err(|e| e.to_string())?;
    let catalog_path = p.lib.join("entries.json");
    if p.refresh_catalog || !catalog_path.exists() {
        let _ = tx.send_blocking(Msg::Phase(
            t("Fetching catalog from Apple…", "Lade Katalog von Apple…").into(),
        ));
        fetch_catalog(&agent, &catalog_path)?;
    }
    let assets = library::load_catalog(&catalog_path)?;

    let dir = library::quality_dir(&p.lib, p.quality);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let have = library::stems_in(&dir);
    let mut seen = std::collections::HashSet::new();
    let mut jobs = VecDeque::new();
    let mut catalog = 0;
    for asset in assets {
        let Some(stem) = asset.stem(p.quality) else {
            continue;
        };
        if !seen.insert(stem.clone()) {
            continue;
        }
        catalog += 1;
        if have.contains(&stem) {
            continue;
        }
        let target = dir.join(format!("{stem}.{}", asset.extension(p.quality)));
        jobs.push_back(Job { asset, target });
    }
    let _ = tx.send_blocking(Msg::Planned {
        todo: jobs.len(),
        catalog,
    });
    if jobs.is_empty() {
        return Ok(());
    }
    if p.quality == Quality::Qhd1440 && !crate::transcode::can_transcode(p.render.as_ref()) {
        return Err(crate::transcode::pipeline_description(
            Path::new(""),
            Path::new(""),
            p.render.as_ref(),
        )
        .unwrap_err()
        .into());
    }
    let _ = tx.send_blocking(Msg::Phase(t("Downloading…", "Lade herunter…").into()));

    let queue = Arc::new(Mutex::new(jobs));
    let transcode_lock = Arc::new(Mutex::new(()));
    let workers = if p.quality == Quality::Qhd1440 {
        p.jobs.min(3)
    } else {
        p.jobs
    };
    let mut threads = Vec::new();
    for _ in 0..workers.max(1) {
        let queue = queue.clone();
        let agent = agent.clone();
        let tx = tx.clone();
        let cancel = cancel.clone();
        let bytes = bytes.clone();
        let lock = transcode_lock.clone();
        let lib = p.lib.clone();
        let quality = p.quality;
        let render = p.render.clone();
        threads.push(std::thread::spawn(move || loop {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let Some(job) = queue.lock().unwrap().pop_front() else {
                return;
            };
            let name = library::label_for(&job.target);
            let res = process(
                &agent,
                &job,
                quality,
                &lib,
                render.as_ref(),
                &cancel,
                &bytes,
                &lock,
            );
            match res {
                Err(Error::Cancelled) => return,
                Ok(()) => {
                    let _ = tx.send_blocking(Msg::ClipDone { name, error: None });
                }
                Err(Error::Other(e)) => {
                    let _ = tx.send_blocking(Msg::ClipDone {
                        name,
                        error: Some(e),
                    });
                }
            }
        }));
    }
    for th in threads {
        let _ = th.join();
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn process(
    agent: &ureq::Agent,
    job: &Job,
    quality: Quality,
    lib: &Path,
    render: Option<&Gpu>,
    cancel: &AtomicBool,
    bytes: &AtomicU64,
    transcode_lock: &Mutex<()>,
) -> Result<(), Error> {
    let url = job.asset.url(quality).ok_or_else(|| "no URL".to_string())?;
    if quality != Quality::Qhd1440 {
        return download(agent, url, &job.target, cancel, bytes);
    }

    let stem = job.asset.stem(quality).unwrap_or_default();
    let uhd_dir = library::quality_dir(lib, Quality::Uhd4k);
    let existing = library::videos_in(&uhd_dir)
        .into_iter()
        .find(|p| p.file_stem().is_some_and(|s| s.to_string_lossy() == stem));
    let (source, temporary) = match existing {
        Some(p) => (p, false),
        None => {
            let cache = lib.join(".cache");
            std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
            let ext = job.asset.extension(Quality::Uhd4k);
            let tmp = cache.join(format!("{stem}.{ext}"));
            download(agent, url, &tmp, cancel, bytes)?;
            (tmp, true)
        }
    };
    let part = part_path(&job.target);
    let result = {
        let _guard = transcode_lock.lock().unwrap_or_else(|e| e.into_inner());
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        crate::transcode::run(&source, &part, render, cancel)
    };
    match result {
        Ok(()) => {
            std::fs::rename(&part, &job.target).map_err(|e| e.to_string())?;
            if temporary {
                let _ = std::fs::remove_file(&source);
            }
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            if e == "cancelled" {
                Err(Error::Cancelled)
            } else {
                Err(Error::Other(format!(
                    "{}: {e}",
                    t("Conversion failed", "Umrechnung fehlgeschlagen")
                )))
            }
        }
    }
}

fn part_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    target.with_file_name(name)
}

fn download(
    agent: &ureq::Agent,
    url: &str,
    target: &Path,
    cancel: &AtomicBool,
    bytes: &AtomicU64,
) -> Result<(), Error> {
    let part = part_path(target);
    let mut last = String::new();
    for attempt in 0..4u64 {
        if attempt > 0 {
            for _ in 0..(attempt * 10) {
                if cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
        match try_download(agent, url, &part, cancel, bytes) {
            Ok(()) => {
                std::fs::rename(&part, target).map_err(|e| e.to_string())?;
                return Ok(());
            }
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(Error::Other(e)) => last = e,
        }
    }
    Err(Error::Other(last))
}

fn content_length(resp: &ureq::http::Response<ureq::Body>) -> Option<u64> {
    resp.headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok())
}

fn try_download(
    agent: &ureq::Agent,
    url: &str,
    part: &Path,
    cancel: &AtomicBool,
    bytes: &AtomicU64,
) -> Result<(), Error> {
    let have = part.metadata().map(|m| m.len()).unwrap_or(0);
    let mut req = agent.get(url);
    if have > 0 {
        req = req.header("Range", format!("bytes={have}-"));
    }
    let resp = req.call().map_err(|e| e.to_string())?;
    let (mut file, start, total) = match resp.status().as_u16() {
        206 => {
            let total = resp
                .headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.rsplit('/').next())
                .and_then(|v| v.trim().parse::<u64>().ok());
            let f = OpenOptions::new()
                .append(true)
                .open(part)
                .map_err(|e| e.to_string())?;
            (f, have, total)
        }
        200 => {
            let total = content_length(&resp);
            let f = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(part)
                .map_err(|e| e.to_string())?;
            (f, 0, total)
        }
        416 => {
            // The part file is probably complete already.
            let head = agent.head(url).call().map_err(|e| e.to_string())?;
            if content_length(&head) == Some(have) {
                return Ok(());
            }
            let _ = std::fs::remove_file(part);
            return Err(Error::Other("HTTP 416".into()));
        }
        s => return Err(Error::Other(format!("HTTP {s}"))),
    };

    let mut reader = resp.into_body().into_reader();
    let mut buf = vec![0u8; 256 * 1024];
    let mut written = start;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        written += n as u64;
        bytes.fetch_add(n as u64, Ordering::Relaxed);
    }
    file.flush().map_err(|e| e.to_string())?;
    match total {
        Some(total) if written != total => Err(Error::Other(format!(
            "incomplete ({written} of {total} bytes)"
        ))),
        _ => Ok(()),
    }
}
