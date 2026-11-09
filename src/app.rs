use crate::config::Config;
use crate::download::{self, Msg};
use crate::gpu::Gpu;
use crate::i18n::t;
use crate::idle::{self, BackendKind};
use crate::{library, settings, system};
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::atomic::Ordering;
use std::time::Instant;

pub use crate::system::APP_ID;

#[derive(Default, Clone)]
pub struct DownloadState {
    pub running: bool,
    pub phase: String,
    pub todo: usize,
    pub catalog: usize,
    pub done: usize,
    pub failed: usize,
    pub last_error: Option<String>,
    pub result: Option<String>,
    pub bytes: u64,
    pub rate: f64,
}

pub struct Core {
    pub app: adw::Application,
    pub cfg: RefCell<Config>,
    pub render: Option<Gpu>,
    pub media_error: RefCell<Option<String>>,
    idle_kind: Cell<BackendKind>,
    settings: RefCell<Option<settings::Window>>,
    dl_hold: RefCell<Option<gio::ApplicationHoldGuard>>,
    pub dl: RefCell<DownloadState>,
    dl_handle: RefCell<Option<download::Handle>>,
    dl_rate: RefCell<(Instant, u64)>,
}

pub fn run(render: Option<Gpu>) -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    let core = Rc::new(Core {
        app: app.clone(),
        cfg: RefCell::new(Config::load()),
        render,
        media_error: RefCell::new(None),
        idle_kind: Cell::new(BackendKind::None),
        settings: RefCell::new(None),
        dl_hold: RefCell::new(None),
        dl: RefCell::new(DownloadState::default()),
        dl_handle: RefCell::new(None),
        dl_rate: RefCell::new((Instant::now(), 0)),
    });

    {
        let core = core.clone();
        app.connect_startup(move |_| core.startup());
    }
    {
        let core = core.clone();
        app.connect_command_line(move |_, _| {
            core.show_settings();
            glib::ExitCode::SUCCESS
        });
    }
    {
        let core = core.clone();
        app.connect_shutdown(move |_| {
            if let Some(h) = core.dl_handle.borrow().as_ref() {
                h.cancel.store(true, Ordering::Relaxed);
            }
        });
    }
    app.run()
}

impl Core {
    fn weak(self: &Rc<Self>) -> Weak<Self> {
        Rc::downgrade(self)
    }

    fn startup(self: &Rc<Self>) {
        if let Err(e) = crate::media::init(self.render.as_ref()) {
            eprintln!("aloft: GStreamer: {e}");
            self.media_error.replace(Some(e));
        }
        self.idle_kind.set(idle::probe());

        let preview = gio::SimpleAction::new("preview", None);
        let w = self.weak();
        preview.connect_activate(move |_, _| {
            if let Some(c) = w.upgrade() {
                c.preview();
            }
        });
        self.app.add_action(&preview);

        if !Config::exists() {
            self.cfg.borrow().save();
        }
        system::sync_autostart(&self.cfg.borrow());
        if self.cfg.borrow().enabled {
            self.ensure_service();
        }
    }

    pub fn idle_backend(&self) -> BackendKind {
        self.idle_kind.get()
    }

    fn ensure_service(&self) {
        if let Err(e) = system::spawn_self(&["--background"]) {
            self.report_error(format!("cannot start the service: {e}"));
        }
    }

    fn preview(&self) {
        let (_, files) = library::playable(&self.cfg.borrow());
        if files.is_empty() {
            self.report_error(
                t(
                    "No videos yet. Download some under \"Videos\" first.",
                    "Noch keine Videos. Lade zuerst welche unter \"Videos\" herunter.",
                )
                .into(),
            );
            return;
        }
        if let Err(e) = system::spawn_self(&["--start"]) {
            self.report_error(e.to_string());
        }
    }

    fn report_error(&self, e: String) {
        eprintln!("aloft: {e}");
        if let Some(win) = self.settings.borrow().as_ref() {
            win.toast(&e);
        }
    }

    pub fn update_config(self: &Rc<Self>, f: impl FnOnce(&mut Config)) {
        let (old, new) = {
            let mut cfg = self.cfg.borrow_mut();
            let old = cfg.clone();
            f(&mut cfg);
            (old, cfg.clone())
        };
        if old == new {
            return;
        }
        new.save();
        if old.autostart != new.autostart {
            system::sync_autostart(&new);
        }
        if new.enabled && !old.enabled {
            self.ensure_service();
        }
    }

    pub fn restart_service(&self) {
        if let Err(e) = system::spawn_self(&["--quit"]) {
            self.report_error(e.to_string());
            return;
        }
        if !self.cfg.borrow().enabled {
            return;
        }
        glib::timeout_add_local_once(std::time::Duration::from_secs(2), || {
            let _ = system::spawn_self(&["--background"]);
        });
    }

    pub fn show_settings(self: &Rc<Self>) {
        if let Some(win) = self.settings.borrow().as_ref() {
            win.window.present();
            return;
        }
        let win = settings::Window::new(self);
        let w = self.weak();
        win.window.connect_close_request(move |_| {
            if let Some(c) = w.upgrade() {
                c.settings.take();
                if c.dl.borrow().running {
                    c.dl_hold.replace(Some(c.app.hold()));
                }
            }
            glib::Propagation::Proceed
        });
        win.window.present();
        self.settings.replace(Some(win));
    }

    pub fn start_download(self: &Rc<Self>, refresh_catalog: bool) {
        if self.dl.borrow().running {
            return;
        }
        let cfg = self.cfg.borrow().clone();
        let handle = download::start(download::Params {
            lib: cfg.library_dir(),
            quality: cfg.quality,
            refresh_catalog,
            jobs: cfg.download_jobs as usize,
            render: self.render.clone(),
        });
        let rx = handle.rx.clone();
        self.dl.replace(DownloadState {
            running: true,
            phase: t("Starting…", "Starte…").into(),
            ..Default::default()
        });
        self.dl_rate.replace((Instant::now(), 0));
        self.dl_handle.replace(Some(handle));
        let w = self.weak();
        glib::spawn_future_local(async move {
            while let Ok(msg) = rx.recv().await {
                let Some(c) = w.upgrade() else { return };
                let finished = matches!(msg, Msg::Finished { .. });
                c.on_download_msg(msg);
                if finished {
                    break;
                }
            }
        });
    }

    pub fn cancel_download(&self) {
        if let Some(h) = self.dl_handle.borrow().as_ref() {
            h.cancel.store(true, Ordering::Relaxed);
            self.dl.borrow_mut().phase = t("Cancelling…", "Breche ab…").into();
        }
    }

    pub fn poll_download(&self) {
        let Some(h) = self.dl_handle.borrow().as_ref().map(|h| h.bytes.clone()) else {
            return;
        };
        let bytes = h.load(Ordering::Relaxed);
        let mut st = self.dl.borrow_mut();
        st.bytes = bytes;
        let mut r = self.dl_rate.borrow_mut();
        let dt = r.0.elapsed().as_secs_f64();
        if dt >= 1.0 {
            let rate = (bytes.saturating_sub(r.1)) as f64 / dt;
            st.rate = if st.rate == 0.0 {
                rate
            } else {
                st.rate * 0.6 + rate * 0.4
            };
            *r = (Instant::now(), bytes);
        }
    }

    fn on_download_msg(&self, msg: Msg) {
        let mut st = self.dl.borrow_mut();
        match msg {
            Msg::Phase(p) => st.phase = p,
            Msg::Planned { todo, catalog } => {
                st.todo = todo;
                st.catalog = catalog;
            }
            Msg::ClipDone { name, error } => {
                st.done += 1;
                if let Some(e) = error {
                    st.failed += 1;
                    eprintln!("aloft: {name}: {e}");
                    st.last_error = Some(format!("{name}: {e}"));
                }
            }
            Msg::Finished { cancelled, error } => {
                st.running = false;
                st.result = Some(if cancelled {
                    t("Download cancelled.", "Download abgebrochen.").to_string()
                } else if let Some(e) = error {
                    e
                } else if st.todo == 0 {
                    t(
                        "All clips are already there.",
                        "Alle Clips sind schon vorhanden.",
                    )
                    .to_string()
                } else if st.failed > 0 {
                    format!(
                        "{} {}, {}",
                        st.failed,
                        t("clips failed", "Clips fehlgeschlagen"),
                        t("just try again later.", "einfach später nochmal versuchen.")
                    )
                } else {
                    t("Download complete.", "Download abgeschlossen.").to_string()
                });
                drop(st);
                self.dl_handle.take();
                self.dl_hold.take();
            }
        }
    }
}
