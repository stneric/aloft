use gtk::{gio, glib};
use std::cell::RefCell;
use std::os::fd::{AsFd, AsRawFd};
use std::rc::{Rc, Weak};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1::{self, ExtIdleNotificationV1},
    ext_idle_notifier_v1::ExtIdleNotifierV1,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum IdleEvent {
    Idle,
    Active,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum BackendKind {
    Wayland,
    Gnome,
    None,
}

type Callback = Rc<dyn Fn(IdleEvent)>;

#[derive(Default)]
struct WlState {
    events: Vec<IdleEvent>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for WlState {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for WlState {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtIdleNotifierV1, ()> for WlState {
    fn event(
        _: &mut Self,
        _: &ExtIdleNotifierV1,
        _: <ExtIdleNotifierV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtIdleNotificationV1, ()> for WlState {
    fn event(
        state: &mut Self,
        _: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_idle_notification_v1::Event::Idled => state.events.push(IdleEvent::Idle),
            ext_idle_notification_v1::Event::Resumed => state.events.push(IdleEvent::Active),
            _ => {}
        }
    }
}

struct Wl {
    conn: Connection,
    queue: EventQueue<WlState>,
    state: WlState,
    seat: wl_seat::WlSeat,
    notifier: ExtIdleNotifierV1,
    notification: Option<ExtIdleNotificationV1>,
    source: Option<glib::SourceId>,
}

impl Wl {
    fn connect() -> Option<Wl> {
        let conn = Connection::connect_to_env().ok()?;
        let (globals, mut queue) = registry_queue_init::<WlState>(&conn).ok()?;
        let qh = queue.handle();
        let notifier: ExtIdleNotifierV1 = globals.bind(&qh, 1..=1, ()).ok()?;
        let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=1, ()).ok()?;
        let mut state = WlState::default();
        queue.roundtrip(&mut state).ok()?;
        Some(Wl {
            conn,
            queue,
            state,
            seat,
            notifier,
            notification: None,
            source: None,
        })
    }

    fn arm(&mut self, timeout_ms: u32) {
        if let Some(n) = self.notification.take() {
            n.destroy();
        }
        let qh = self.queue.handle();
        self.notification =
            Some(
                self.notifier
                    .get_idle_notification(timeout_ms, &self.seat, &qh, ()),
            );
        let _ = self.conn.flush();
    }

    fn pump(&mut self) -> Result<Vec<IdleEvent>, ()> {
        let Wl {
            queue, state, conn, ..
        } = self;
        queue.dispatch_pending(state).map_err(|_| ())?;
        if let Some(guard) = queue.prepare_read() {
            match guard.read() {
                Ok(_) => {}
                Err(wayland_client::backend::WaylandError::Io(e))
                    if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return Err(()),
            }
        }
        queue.dispatch_pending(state).map_err(|_| ())?;
        let _ = conn.flush();
        Ok(std::mem::take(&mut state.events))
    }
}

impl Drop for Wl {
    fn drop(&mut self) {
        if let Some(s) = self.source.take() {
            s.remove();
        }
        if let Some(n) = self.notification.take() {
            n.destroy();
        }
        self.notifier.destroy();
        let _ = self.conn.flush();
    }
}

const MUTTER: &str = "org.gnome.Mutter.IdleMonitor";
const MUTTER_PATH: &str = "/org/gnome/Mutter/IdleMonitor/Core";

struct Gnome {
    bus: gio::DBusConnection,
    idle_watch: Option<u32>,
    active_watch: Option<u32>,
    signal: Option<gio::SignalSubscriptionId>,
}

impl Gnome {
    fn connect() -> Option<Gnome> {
        let bus = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok()?;
        let owner = bus
            .call_sync(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "NameHasOwner",
                Some(&(MUTTER,).into()),
                Some(glib::VariantTy::new("(b)").unwrap()),
                gio::DBusCallFlags::NONE,
                2000,
                gio::Cancellable::NONE,
            )
            .ok()?;
        if !owner.get::<(bool,)>()?.0 {
            return None;
        }
        Some(Gnome {
            bus,
            idle_watch: None,
            active_watch: None,
            signal: None,
        })
    }

    fn call_u32(&self, method: &str, args: Option<glib::Variant>) -> Option<u32> {
        self.bus
            .call_sync(
                Some(MUTTER),
                MUTTER_PATH,
                MUTTER,
                method,
                args.as_ref(),
                Some(glib::VariantTy::new("(u)").unwrap()),
                gio::DBusCallFlags::NONE,
                2000,
                gio::Cancellable::NONE,
            )
            .ok()
            .and_then(|v| v.get::<(u32,)>())
            .map(|v| v.0)
    }

    fn remove(&mut self, id: Option<u32>) {
        if let Some(id) = id {
            let _ = self.bus.call_sync(
                Some(MUTTER),
                MUTTER_PATH,
                MUTTER,
                "RemoveWatch",
                Some(&(id,).into()),
                None,
                gio::DBusCallFlags::NONE,
                2000,
                gio::Cancellable::NONE,
            );
        }
    }

    fn arm(&mut self, timeout_ms: u32) {
        let old = self.idle_watch.take();
        self.remove(old);
        self.idle_watch = self.call_u32("AddIdleWatch", Some((timeout_ms as u64,).into()));
    }

    fn arm_active(&mut self) {
        let old = self.active_watch.take();
        self.remove(old);
        self.active_watch = self.call_u32("AddUserActiveWatch", None);
    }

    /// GNOME's idle monitor ignores inhibitors, so ask the session manager.
    fn inhibited(&self) -> bool {
        self.bus
            .call_sync(
                Some("org.gnome.SessionManager"),
                "/org/gnome/SessionManager",
                "org.gnome.SessionManager",
                "IsInhibited",
                Some(&(8u32,).into()),
                Some(glib::VariantTy::new("(b)").unwrap()),
                gio::DBusCallFlags::NONE,
                2000,
                gio::Cancellable::NONE,
            )
            .ok()
            .and_then(|v| v.get::<(bool,)>())
            .is_some_and(|v| v.0)
    }
}

impl Drop for Gnome {
    fn drop(&mut self) {
        if let Some(s) = self.signal.take() {
            #[allow(deprecated)]
            self.bus.signal_unsubscribe(s);
        }
        let a = self.idle_watch.take();
        self.remove(a);
        let b = self.active_watch.take();
        self.remove(b);
    }
}

enum Backend {
    Wayland(Wl),
    Gnome(Gnome),
    None,
}

struct Inner {
    backend: RefCell<Backend>,
    cb: Callback,
}

pub struct Watcher(Rc<Inner>);

impl Watcher {
    pub fn new(timeout_ms: u32, cb: impl Fn(IdleEvent) + 'static) -> Watcher {
        let backend = if let Some(wl) = Wl::connect() {
            Backend::Wayland(wl)
        } else if let Some(g) = Gnome::connect() {
            Backend::Gnome(g)
        } else {
            Backend::None
        };
        if crate::media::debug() {
            eprintln!(
                "aloft: idle backend {}",
                match &backend {
                    Backend::Wayland(_) => "wayland",
                    Backend::Gnome(_) => "gnome",
                    Backend::None => "none",
                }
            );
        }
        let w = Watcher(Rc::new(Inner {
            backend: RefCell::new(backend),
            cb: Rc::new(cb),
        }));
        w.hook_up();
        w.set_timeout(timeout_ms);
        w
    }

    pub fn kind(&self) -> BackendKind {
        match &*self.0.backend.borrow() {
            Backend::Wayland(_) => BackendKind::Wayland,
            Backend::Gnome(_) => BackendKind::Gnome,
            Backend::None => BackendKind::None,
        }
    }

    pub fn set_timeout(&self, timeout_ms: u32) {
        match &mut *self.0.backend.borrow_mut() {
            Backend::Wayland(wl) => wl.arm(timeout_ms),
            Backend::Gnome(g) => g.arm(timeout_ms),
            Backend::None => {}
        }
    }

    fn hook_up(&self) {
        let weak: Weak<Inner> = Rc::downgrade(&self.0);
        match &mut *self.0.backend.borrow_mut() {
            Backend::Wayland(wl) => {
                let fd = wl.conn.as_fd().as_raw_fd();
                let src = glib::source::unix_fd_add_local(
                    fd,
                    glib::IOCondition::IN | glib::IOCondition::ERR | glib::IOCondition::HUP,
                    move |_, cond| {
                        let Some(inner) = weak.upgrade() else {
                            return glib::ControlFlow::Break;
                        };
                        let result = match &mut *inner.backend.borrow_mut() {
                            Backend::Wayland(wl)
                                if !cond.intersects(
                                    glib::IOCondition::ERR | glib::IOCondition::HUP,
                                ) =>
                            {
                                wl.pump()
                            }
                            _ => Err(()),
                        };
                        match result {
                            Ok(events) => {
                                for e in events {
                                    (inner.cb)(e);
                                }
                                glib::ControlFlow::Continue
                            }
                            Err(()) => {
                                eprintln!("aloft: lost the Wayland idle connection");
                                // Returning Break drops the source; Drop must not remove it again.
                                if let Backend::Wayland(wl) = &mut *inner.backend.borrow_mut() {
                                    wl.source = None;
                                }
                                inner.backend.replace(Backend::None);
                                glib::ControlFlow::Break
                            }
                        }
                    },
                );
                wl.source = Some(src);
            }
            Backend::Gnome(g) => {
                #[allow(deprecated)]
                let id = g.bus.signal_subscribe(
                    Some(MUTTER),
                    Some(MUTTER),
                    Some("WatchFired"),
                    Some(MUTTER_PATH),
                    None,
                    gio::DBusSignalFlags::NONE,
                    move |_, _, _, _, _, params| {
                        let Some(inner) = weak.upgrade() else { return };
                        let Some((id,)) = params.get::<(u32,)>() else {
                            return;
                        };
                        let event = {
                            let mut b = inner.backend.borrow_mut();
                            let Backend::Gnome(g) = &mut *b else { return };
                            if Some(id) == g.idle_watch {
                                if g.inhibited() {
                                    None
                                } else {
                                    g.arm_active();
                                    Some(IdleEvent::Idle)
                                }
                            } else if Some(id) == g.active_watch {
                                g.active_watch = None;
                                Some(IdleEvent::Active)
                            } else {
                                None
                            }
                        };
                        if let Some(e) = event {
                            (inner.cb)(e);
                        }
                    },
                );
                g.signal = Some(id);
            }
            Backend::None => {}
        }
    }
}

pub fn probe() -> BackendKind {
    if Wl::connect().is_some() {
        BackendKind::Wayland
    } else if Gnome::connect().is_some() {
        BackendKind::Gnome
    } else {
        BackendKind::None
    }
}
