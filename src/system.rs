use crate::config::Config;
use crate::i18n::t;
use std::path::PathBuf;

pub const APP_ID: &str = "de.eric_s.Aloft";

pub fn executable() -> PathBuf {
    if let Some(p) = std::env::var_os("APPIMAGE") {
        return PathBuf::from(p);
    }
    match std::env::current_exe() {
        // After a package update the running file is gone; its path is still
        // the right one to start again.
        Ok(p) => {
            let s = p.to_string_lossy();
            match s.strip_suffix(" (deleted)") {
                Some(orig) => PathBuf::from(orig),
                None => p,
            }
        }
        Err(_) => PathBuf::from("aloft"),
    }
}

pub fn spawn_self(args: &[&str]) -> std::io::Result<()> {
    let mut cmd = std::process::Command::new(executable());
    cmd.args(args).stdin(std::process::Stdio::null());
    let mut child = cmd.spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn autostart_path() -> PathBuf {
    gtk::glib::user_config_dir()
        .join("autostart")
        .join(format!("{APP_ID}.desktop"))
}

/// Rewrites the autostart entry, since an AppImage can move.
pub fn sync_autostart(cfg: &Config) {
    let path = autostart_path();
    if !cfg.autostart {
        let _ = std::fs::remove_file(&path);
        return;
    }
    let exe = executable().to_string_lossy().into_owned();
    let quoted = exe
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('`', "\\`")
        .replace('$', "\\$");
    let content = format!(
        "[Desktop Entry]\nType=Application\nName=Aloft\nComment={}\nExec=\"{quoted}\" --background\nIcon={APP_ID}\nTerminal=false\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\nX-KDE-autostart-phase=2\n",
        t("Aerial screensaver", "Aerial-Bildschirmschoner"),
    );
    if std::fs::read_to_string(&path).ok().as_deref() == Some(content.as_str()) {
        return;
    }
    let res = path
        .parent()
        .map(std::fs::create_dir_all)
        .unwrap_or(Ok(()))
        .and_then(|_| std::fs::write(&path, content));
    if let Err(e) = res {
        eprintln!("aloft: cannot write {}: {e}", path.display());
    }
}
