use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quality {
    #[serde(rename = "1080p")]
    Hd1080,
    #[serde(rename = "1440p")]
    Qhd1440,
    #[serde(rename = "4k")]
    Uhd4k,
}

impl Quality {
    pub const ALL: [Quality; 3] = [Quality::Hd1080, Quality::Qhd1440, Quality::Uhd4k];

    pub fn dir_name(self) -> &'static str {
        match self {
            Quality::Hd1080 => "1080p",
            Quality::Qhd1440 => "1440p",
            Quality::Uhd4k => "4K",
        }
    }

    pub fn catalog_keys(self) -> &'static [&'static str] {
        match self {
            Quality::Hd1080 => &["url-1080-SDR", "url-1080-H264"],
            Quality::Qhd1440 | Quality::Uhd4k => &["url-4K-SDR", "url-1080-SDR", "url-1080-H264"],
        }
    }

    pub fn index(self) -> u32 {
        Quality::ALL.iter().position(|q| *q == self).unwrap_or(2) as u32
    }

    pub fn from_index(i: u32) -> Quality {
        Quality::ALL
            .get(i as usize)
            .copied()
            .unwrap_or(Quality::Uhd4k)
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub idle_minutes: u32,
    pub stop_after_minutes: u32,
    pub stop_on_lock: bool,
    pub same_on_all: bool,
    pub show_labels: bool,
    pub fades: bool,
    pub quality: Quality,
    pub library_dir: Option<PathBuf>,
    /// "auto" or a /dev/dri/by-path id such as "pci-0000:00:02.0".
    pub gpu: String,
    pub autostart: bool,
    pub download_jobs: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            idle_minutes: 5,
            stop_after_minutes: 60,
            stop_on_lock: true,
            same_on_all: false,
            show_labels: true,
            fades: true,
            quality: Quality::Uhd4k,
            library_dir: None,
            gpu: "auto".into(),
            autostart: true,
            download_jobs: 4,
        }
    }
}

pub fn config_dir() -> PathBuf {
    gtk::glib::user_config_dir().join("aloft")
}

fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

impl Config {
    pub fn load() -> Config {
        let mut cfg: Config = std::fs::read_to_string(config_path())
            .ok()
            .and_then(|s| match serde_json::from_str(&s) {
                Ok(c) => Some(c),
                Err(e) => {
                    eprintln!("aloft: ignoring broken config: {e}");
                    None
                }
            })
            .unwrap_or_default();
        cfg.idle_minutes = cfg.idle_minutes.clamp(1, 240);
        cfg.stop_after_minutes = cfg.stop_after_minutes.min(1440);
        cfg.download_jobs = cfg.download_jobs.clamp(1, 8);
        cfg
    }

    pub fn exists() -> bool {
        config_path().exists()
    }

    pub fn save(&self) {
        let dir = config_dir();
        let tmp = dir.join("config.json.tmp");
        let res = std::fs::create_dir_all(&dir)
            .and_then(|_| std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap()))
            .and_then(|_| std::fs::rename(&tmp, config_path()));
        if let Err(e) = res {
            eprintln!("aloft: could not save config: {e}");
        }
    }

    pub fn library_dir(&self) -> PathBuf {
        self.library_dir
            .clone()
            .unwrap_or_else(|| gtk::glib::user_data_dir().join("aloft"))
    }
}
