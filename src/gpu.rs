use crate::i18n::t;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Gpu {
    /// /dev/dri/by-path name without "-render", e.g. "pci-0000:00:02.0".
    pub id: String,
    pub node: PathBuf,
    pub vendor: u32,
    pub name: String,
    pub integrated: bool,
}

impl Gpu {
    pub fn label(&self) -> String {
        let kind = if self.integrated {
            t("integrated", "integriert")
        } else {
            t("dedicated", "dediziert")
        };
        format!("{} ({kind})", self.name)
    }
}

fn read_hex(path: &Path) -> u32 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
        .unwrap_or(0)
}

fn vendor_name(vendor: u32) -> &'static str {
    match vendor {
        0x8086 => "Intel",
        0x1002 => "AMD",
        0x10de => "NVIDIA",
        _ => "GPU",
    }
}

/// "GA104 [GeForce RTX 3070 Ti]" -> "NVIDIA GeForce RTX 3070 Ti", from hwdata's pci.ids.
fn device_name(vendor: u32, device: u32) -> String {
    let fallback = format!("{} GPU", vendor_name(vendor));
    let ids = [
        "/usr/share/hwdata/pci.ids",
        "/usr/share/misc/pci.ids",
        "/usr/share/pci.ids",
    ]
    .iter()
    .find_map(|p| std::fs::read_to_string(p).ok());
    let Some(ids) = ids else { return fallback };
    let vendor_tag = format!("{vendor:04x} ");
    let device_tag = format!("\t{device:04x} ");
    let mut in_vendor = false;
    for line in ids.lines() {
        if !line.starts_with('\t') && !line.starts_with('#') {
            in_vendor = line.starts_with(&vendor_tag);
            continue;
        }
        if in_vendor && line.starts_with(&device_tag) {
            let raw = line[device_tag.len()..].trim();
            let model = match (raw.find('['), raw.rfind(']')) {
                (Some(a), Some(b)) if a < b => &raw[a + 1..b],
                _ => raw,
            };
            return format!("{} {model}", vendor_name(vendor));
        }
    }
    fallback
}

pub fn list() -> Vec<Gpu> {
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir("/dev/dri/by-path") else {
        return out;
    };
    for entry in dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(id) = name.strip_suffix("-render") else {
            continue;
        };
        let Ok(node) = std::fs::canonicalize(entry.path()) else {
            continue;
        };
        let Some(node_name) = node.file_name() else {
            continue;
        };
        let sys = Path::new("/sys/class/drm").join(node_name).join("device");
        let vendor = read_hex(&sys.join("vendor"));
        let device = read_hex(&sys.join("device"));
        let vram: Option<u64> = std::fs::read_to_string(sys.join("mem_info_vram_total"))
            .ok()
            .and_then(|s| s.trim().parse().ok());
        let integrated = match vendor {
            // Arc cards sit behind a PCIe bridge, iGPUs on the root bus.
            0x8086 => id.starts_with("pci-0000:00:"),
            0x1002 => vram.is_some_and(|v| v <= 4 << 30),
            _ => false,
        };
        out.push(Gpu {
            id: id.to_string(),
            node,
            vendor,
            name: device_name(vendor, device),
            integrated,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// `pref` is "auto" or a by-path id. None means: leave it to GStreamer.
pub fn select(pref: &str) -> Option<Gpu> {
    let all = list();
    if all.len() < 2 {
        return None;
    }
    if let Some(i) = all.iter().position(|g| g.id == pref) {
        return Some(all[i].clone());
    }
    all.into_iter().find(|g| g.integrated)
}
