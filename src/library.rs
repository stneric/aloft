use crate::config::{Config, Quality};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub const CATALOG_URL: &str = "https://sylvan.apple.com/Aerials/resources-15.tar";
const VIDEO_EXTS: [&str; 3] = ["mov", "mp4", "m4v"];

#[derive(Deserialize, Clone, Debug)]
pub struct Asset {
    #[serde(rename = "accessibilityLabel", default)]
    pub label: Option<String>,
    #[serde(rename = "shotID", default)]
    pub shot: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct CatalogFile {
    assets: Vec<Asset>,
}

impl Asset {
    pub fn url(&self, q: Quality) -> Option<&str> {
        q.catalog_keys()
            .iter()
            .find_map(|k| self.extra.get(*k).and_then(|v| v.as_str()))
    }

    pub fn stem(&self, q: Quality) -> Option<String> {
        let url = self.url(q)?;
        let label = self.label.as_deref().unwrap_or("Aerial");
        let shot = match &self.shot {
            Some(s) => s.clone(),
            None => {
                let file = url.rsplit('/').next().unwrap_or(url);
                file.rsplit_once('.').map_or(file, |(a, _)| a).to_string()
            }
        };
        let stem: String = format!("{label} ~ {shot}")
            .chars()
            .map(|c| if c == '/' || c == '\0' { '-' } else { c })
            .collect();
        Some(stem.trim().to_string())
    }

    pub fn extension(&self, q: Quality) -> String {
        if q == Quality::Qhd1440 {
            return "mp4".into();
        }
        self.url(q)
            .and_then(|u| u.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()))
            .filter(|e| e.len() <= 4)
            .unwrap_or_else(|| "mov".into())
    }
}

pub fn catalog_path(cfg: &Config) -> PathBuf {
    cfg.library_dir().join("entries.json")
}

pub fn quality_dir(lib: &Path, q: Quality) -> PathBuf {
    lib.join(q.dir_name())
}

pub fn parse_catalog(json: &[u8]) -> Result<Vec<Asset>, String> {
    let file: CatalogFile = serde_json::from_slice(json).map_err(|e| e.to_string())?;
    Ok(file.assets)
}

pub fn load_catalog(path: &Path) -> Result<Vec<Asset>, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    parse_catalog(&data)
}

fn is_video(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| VIDEO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

pub fn videos_in(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_video(p) && p.is_file())
        .collect();
    out.sort();
    out
}

pub fn stems_in(dir: &Path) -> HashSet<String> {
    videos_in(dir)
        .iter()
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect()
}

pub fn stats(dir: &Path) -> (usize, u64) {
    let vids = videos_in(dir);
    let bytes = vids
        .iter()
        .filter_map(|p| p.metadata().ok())
        .map(|m| m.len())
        .sum();
    (vids.len(), bytes)
}

pub fn playable(cfg: &Config) -> (Quality, Vec<PathBuf>) {
    let lib = cfg.library_dir();
    let order: &[Quality] = match cfg.quality {
        Quality::Hd1080 => &[Quality::Hd1080, Quality::Qhd1440, Quality::Uhd4k],
        Quality::Qhd1440 => &[Quality::Qhd1440, Quality::Uhd4k, Quality::Hd1080],
        Quality::Uhd4k => &[Quality::Uhd4k, Quality::Qhd1440, Quality::Hd1080],
    };
    for q in order {
        let vids = videos_in(&quality_dir(&lib, *q));
        if !vids.is_empty() {
            return (*q, vids);
        }
    }
    (cfg.quality, Vec::new())
}

pub fn label_for(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    match stem.split_once(" ~ ") {
        Some((label, _)) => label.trim().to_string(),
        None => stem,
    }
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1000.0 && i < UNITS.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}
