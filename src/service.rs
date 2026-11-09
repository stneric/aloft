use crate::config::Config;
use crate::gpu::Gpu;
use crate::idle::{self, BackendKind, IdleEvent};
use crate::library;
use crate::saver::{self, Session};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

pub const SERVICE_ID: &str = "de.eric_s.Aloft.Service";

pub struct Service {
    app: gtk::Application,
    cfg: RefCell<Config>,
    render: Option<Gpu>,
    media_error: RefCell<Option<String>>,
    idle: RefCell<Option<idle::Watcher>>,
    session: RefCell<Option<Session>>,
    session_manual: Cell<bool>,
    hold: RefCell<Option<gio::ApplicationHoldGuard>>,
    locked: Cell<bool>,
    fresh: Cell<bool>,
    signals: RefCell<Vec<gio::SignalSubscriptionId>>,
    bus: RefCell<Option<gio::DBusConnection>>,
    config_monitor: RefCell<Option<gio::FileMonitor>>,
    reload_pending: Cell<bool>,
}

fn idle_ms(minutes: u32) -> u32 {
    minutes * 60_000
}

/// True when the executable was replaced on disk (package update). Not for
/// AppImages, which run from their own mount.
fn binary_replaced() -> bool {
    std::env::var_os("APPIMAGE").is_none()
        && std::fs::read_link("/proc/self/exe").is_ok_and(|p| p.to_string_lossy().ends_with(" (deleted)"))
}

pub fn run(render: Option<Gpu>) -> glib::ExitCode {
    let app = gtk::Application::builder()
        .application_id(SERVICE_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    let svc = Rc::new(Service {
        app: app.clone(),
        cfg: RefCell::new(Config::load()),
        render,
        media_error: RefCell::new(None),
        idle: RefCell::new(None),
        session: RefCell::new(None),
        session_manual: Cell::new(false),
        hold: RefCell::new(None),
        locked: Cell::new(false),
        fresh: Cell::new(true),
        signals: RefCell::new(Vec::new()),
        bus: RefCell::new(None),
        config_monitor: RefCell::new(None),
        reload_pending: Cell::new(false),
    });
    {
        let svc = svc.clone();
        app.connect_startup(move |_| svc.startup());
    }
    {
        let svc = svc.clone();
        app.connect_command_line(move |_, cl| {
            let args: Vec<String> = cl
                .arguments()
                .iter()
                .skip(1)
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            svc.command(&args);
            glib::ExitCode::SUCCESS
        });
    }
    {
        let svc = svc.clone();
        app.connect_shutdown(move |_| svc.shutdown());
    }
    app.run()
}

impl Service {
    fn weak(self: &Rc<Self>) -> Weak<Self> {
        Rc::downgrade(self)
    }

    fn startup(self: &Rc<Self>) {
        self.hold.replace(Some(self.app.hold()));

        let css = gtk::CssProvider::new();
        css.load_from_string(saver::CSS);
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        if let Err(e) = crate::media::init(self.render.as_ref()) {
            eprintln!("aloft: GStreamer: {e}");
            self.media_error.replace(Some(e));
        }
        self.setup_idle();
        self.setup_lock_monitor();
        self.setup_monitor_watch();
        self.watch_config();
        crate::system::sync_autostart(&self.cfg.borrow());

        // After a package update this process still runs the old program.
        let w = self.weak();
        glib::timeout_add_seconds_local(60, move || match w.upgrade() {
            Some(s) => {
                if s.session.borrow().is_none() && binary_replaced() {
                    s.restart();
                }
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
    }

    /// Hands over to a fresh copy of the (updated) program.
    fn restart(&self) {
        eprintln!("aloft: program was updated, restarting the service");
        let spawned = std::process::Command::new("/bin/sh")
            // Wait until this instance has released its bus name, or the new
            // one would just hand over to us again.
            .args([
                "-c",
                "for i in $(seq 50); do \
                   gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
                     --method org.freedesktop.DBus.NameHasOwner \"$1\" 2>/dev/null | grep -q true || break; \
                   sleep 0.2; \
                 done; exec \"$0\" --background",
            ])
            .arg(crate::system::executable())
            .arg(SERVICE_ID)
            .stdin(std::process::Stdio::null())
            .spawn();
        if spawned.is_ok() {
            self.quit();
        }
    }

    fn shutdown(&self) {
        self.session.take();
        if let Some(bus) = self.bus.borrow().as_ref() {
            for id in self.signals.take() {
                #[allow(deprecated)]
                bus.signal_unsubscribe(id);
            }
        }
        self.idle.take();
        self.config_monitor.take();
    }

    fn command(self: &Rc<Self>, args: &[String]) {
        // `--stop` without a running service would otherwise leave one behind.
        let fresh = self.fresh.replace(false);
        for a in args {
            match a.as_str() {
                "--background" | "-b" => {
                    // Someone (autostart, the settings) wants the service: make
                    // sure it is the current version.
                    if !fresh && self.session.borrow().is_none() && binary_replaced() {
                        self.restart();
                        return;
                    }
                }
                "--start" | "--now" => self.start_session(true),
                "--stop" => {
                    self.stop_session();
                    if fresh {
                        self.quit();
                    }
                }
                "--quit" => self.quit(),
                other => eprintln!("aloft: unknown option {other}"),
            }
        }
    }

    fn quit(&self) {
        self.session.take();
        self.hold.take();
        self.app.quit();
    }

    fn watch_config(self: &Rc<Self>) {
        let file = gio::File::for_path(crate::config::config_dir().join("config.json"));
        let Ok(monitor) = file.monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
        else {
            return;
        };
        let w = self.weak();
        monitor.connect_changed(move |_, _, _, _| {
            let Some(s) = w.upgrade() else { return };
            if s.reload_pending.replace(true) {
                return;
            }
            let w = Rc::downgrade(&s);
            glib::timeout_add_local_once(Duration::from_millis(300), move || {
                if let Some(s) = w.upgrade() {
                    s.reload_pending.set(false);
                    s.reload_config();
                }
            });
        });
        self.config_monitor.replace(Some(monitor));
    }

    fn reload_config(&self) {
        let new = Config::load();
        let old = self.cfg.replace(new.clone());
        if old == new {
            return;
        }
        if crate::media::debug() {
            eprintln!("aloft: config reloaded");
        }
        if old.idle_minutes != new.idle_minutes {
            if let Some(w) = self.idle.borrow().as_ref() {
                w.set_timeout(idle_ms(new.idle_minutes));
            }
        }
        if !new.enabled && !self.session_manual.get() {
            self.stop_session();
        }
    }

    fn setup_idle(self: &Rc<Self>) {
        let w = self.weak();
        let ms = idle_ms(self.cfg.borrow().idle_minutes);
        let watcher = idle::Watcher::new(ms, move |ev| {
            let Some(s) = w.upgrade() else { return };
            if crate::media::debug() {
                eprintln!("aloft: idle event {ev:?}");
            }
            match ev {
                IdleEvent::Idle => {
                    if s.cfg.borrow().enabled && !s.locked.get() {
                        s.start_session(false);
                    }
                }
                IdleEvent::Active => {
                    if !s.session_manual.get() {
                        s.stop_session();
                    }
                }
            }
        });
        if watcher.kind() == BackendKind::None {
            eprintln!("aloft: no idle detection available (needs ext-idle-notify-v1 or GNOME)");
        }
        self.idle.replace(Some(watcher));
    }

    fn setup_lock_monitor(self: &Rc<Self>) {
        let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
            return;
        };
        let mut ids = Vec::new();
        for iface in ["org.freedesktop.ScreenSaver", "org.gnome.ScreenSaver"] {
            let w = self.weak();
            #[allow(deprecated)]
            let id = bus.signal_subscribe(
                None,
                Some(iface),
                Some("ActiveChanged"),
                None,
                None,
                gio::DBusSignalFlags::NONE,
                move |_, _, _, _, _, params| {
                    let Some(s) = w.upgrade() else { return };
                    let Some((active,)) = params.get::<(bool,)>() else {
                        return;
                    };
                    s.locked.set(active);
                    if active && s.cfg.borrow().stop_on_lock {
                        s.stop_session();
                    }
                },
            );
            ids.push(id);
        }
        self.signals.replace(ids);
        self.bus.replace(Some(bus));
    }

    fn setup_monitor_watch(self: &Rc<Self>) {
        let Some(display) = gdk::Display::default() else {
            return;
        };
        let w = self.weak();
        display.monitors().connect_items_changed(move |_, _, _, _| {
            let Some(s) = w.upgrade() else { return };
            if s.session.borrow().is_some() {
                let manual = s.session_manual.get();
                s.stop_session();
                let w = Rc::downgrade(&s);
                glib::timeout_add_local_once(Duration::from_millis(500), move || {
                    if let Some(s) = w.upgrade() {
                        s.start_session(manual);
                    }
                });
            }
        });
    }

    fn start_session(self: &Rc<Self>, manual: bool) {
        if self.session.borrow().is_some() {
            return;
        }
        if let Some(e) = self.media_error.borrow().as_ref() {
            eprintln!("aloft: {e}");
            return;
        }
        let cfg = self.cfg.borrow().clone();
        let (_, files) = library::playable(&cfg);
        if files.is_empty() {
            eprintln!("aloft: no videos in {}", cfg.library_dir().display());
            return;
        }
        let w = self.weak();
        let on_end: Rc<dyn Fn(Option<String>)> = Rc::new(move |err| {
            if let Some(s) = w.upgrade() {
                s.stop_session();
                if let Some(e) = err {
                    eprintln!("aloft: {e}");
                }
            }
        });
        match Session::start(&self.app, &cfg, files, manual, self.render.as_ref(), on_end) {
            Ok(session) => {
                self.session_manual.set(manual);
                self.session.replace(Some(session));
            }
            Err(e) => eprintln!("aloft: {e}"),
        }
    }

    fn stop_session(&self) {
        if crate::media::debug() && self.session.borrow().is_some() {
            eprintln!("aloft: stop_session");
        }
        // Take it out before dropping: closing the windows calls back into us.
        let s = self.session.take();
        drop(s);
        self.session_manual.set(false);
    }
}
