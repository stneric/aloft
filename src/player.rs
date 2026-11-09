use crate::library::label_for;
use gst::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

pub const FADE: Duration = Duration::from_millis(1500);

pub enum PlayerEvent {
    Started(String),
    FadeOut,
    Failed(String),
}

type Handler = Rc<dyn Fn(PlayerEvent)>;

struct Inner {
    playbin: gst::Element,
    sink: gst::Element,
    ticks: Cell<u32>,
    paintable: gdk::Paintable,
    files: RefCell<Vec<PathBuf>>,
    pos: Cell<usize>,
    fades: bool,
    faded: Cell<bool>,
    errors: Cell<usize>,
    last_error: RefCell<String>,
    stopped: Cell<bool>,
    handler: RefCell<Option<Handler>>,
    bus_watch: RefCell<Option<gst::bus::BusWatchGuard>>,
    timer: RefCell<Option<glib::SourceId>>,
}

#[derive(Clone)]
pub struct Player(Rc<Inner>);

impl Player {
    pub fn new(
        mut files: Vec<PathBuf>,
        fades: bool,
        render: Option<&crate::gpu::Gpu>,
    ) -> Result<Player, String> {
        if files.is_empty() {
            return Err("no videos".into());
        }
        fastrand::shuffle(&mut files);
        let (video_sink, sink) = crate::media::video_sink(render)?;
        let paintable = sink.property::<gdk::Paintable>("paintable");
        let playbin = gst::ElementFactory::make("playbin3")
            .property("video-sink", &video_sink)
            .build()
            .map_err(|e| format!("playbin3: {e}"))?;
        playbin.set_property_from_str("flags", "video");

        let p = Player(Rc::new(Inner {
            playbin,
            sink: sink.clone(),
            ticks: Cell::new(0),
            paintable,
            files: RefCell::new(files),
            pos: Cell::new(0),
            fades,
            faded: Cell::new(false),
            errors: Cell::new(0),
            last_error: RefCell::new(String::new()),
            stopped: Cell::new(false),
            handler: RefCell::new(None),
            bus_watch: RefCell::new(None),
            timer: RefCell::new(None),
        }));

        let weak = Rc::downgrade(&p.0);
        let watch =
            p.0.playbin
                .bus()
                .unwrap()
                .add_watch_local(move |_, msg| {
                    if let Some(inner) = weak.upgrade() {
                        Player(inner).on_message(msg);
                    }
                    glib::ControlFlow::Continue
                })
                .map_err(|e| e.to_string())?;
        p.0.bus_watch.replace(Some(watch));

        let weak = Rc::downgrade(&p.0);
        let timer =
            glib::timeout_add_local(Duration::from_millis(200), move || match weak.upgrade() {
                Some(inner) => {
                    Player(inner).tick();
                    glib::ControlFlow::Continue
                }
                None => glib::ControlFlow::Break,
            });
        p.0.timer.replace(Some(timer));
        Ok(p)
    }

    pub fn paintable(&self) -> gdk::Paintable {
        self.0.paintable.clone()
    }

    pub fn connect(&self, f: impl Fn(PlayerEvent) + 'static) {
        self.0.handler.replace(Some(Rc::new(f)));
    }

    fn emit(&self, ev: PlayerEvent) {
        // Clone first: the handler may stop (and thereby reset) this player.
        let handler = self.0.handler.borrow().clone();
        if let Some(h) = handler {
            h(ev);
        }
    }

    pub fn start(&self) {
        self.load();
    }

    pub fn stop(&self) {
        if self.0.stopped.replace(true) {
            return;
        }
        let _ = self.0.playbin.set_state(gst::State::Null);
        if let Some(t) = self.0.timer.take() {
            t.remove();
        }
        self.0.bus_watch.take();
        self.0.handler.take();
    }

    fn current(&self) -> PathBuf {
        self.0.files.borrow()[self.0.pos.get()].clone()
    }

    fn load(&self) {
        if self.0.stopped.get() {
            return;
        }
        let path = self.current();
        let uri = match glib::filename_to_uri(&path, None) {
            Ok(u) => u,
            Err(e) => {
                self.fail(format!("{}: {e}", path.display()));
                return;
            }
        };
        let pb = &self.0.playbin;
        let _ = pb.set_state(gst::State::Ready);
        pb.set_property("uri", uri.as_str());
        self.0.faded.set(false);
        if pb.set_state(gst::State::Playing).is_err() {
            self.fail(format!("{}: cannot play", path.display()));
        }
    }

    fn advance(&self) {
        let len = self.0.files.borrow().len();
        let next = self.0.pos.get() + 1;
        if next >= len {
            let last = self.current();
            let mut files = self.0.files.borrow_mut();
            fastrand::shuffle(&mut files);
            if files.len() > 1 && files[0] == last {
                files.swap(0, 1);
            }
            self.0.pos.set(0);
        } else {
            self.0.pos.set(next);
        }
        self.load();
    }

    fn fail(&self, err: String) {
        eprintln!("aloft: {err}");
        self.0.last_error.replace(err.clone());
        let n = self.0.errors.get() + 1;
        self.0.errors.set(n);
        if n >= self.0.files.borrow().len().min(10) {
            self.stop_and_report(err);
            return;
        }
        // Defer, so we never re-enter GStreamer from its own callback.
        let weak = Rc::downgrade(&self.0);
        glib::idle_add_local_once(move || {
            if let Some(inner) = weak.upgrade() {
                Player(inner).advance();
            }
        });
    }

    fn stop_and_report(&self, err: String) {
        let handler = self.0.handler.take();
        self.stop();
        if let Some(h) = handler {
            h(PlayerEvent::Failed(err));
        }
    }

    fn on_message(&self, msg: &gst::Message) {
        use gst::MessageView;
        match msg.view() {
            MessageView::Eos(_) => {
                let weak = Rc::downgrade(&self.0);
                glib::idle_add_local_once(move || {
                    if let Some(inner) = weak.upgrade() {
                        Player(inner).advance();
                    }
                });
            }
            MessageView::Error(e) => {
                let src = e
                    .src()
                    .map(|s| s.path_string().to_string())
                    .unwrap_or_default();
                self.fail(format!(
                    "{}: {} ({src})",
                    self.current().display(),
                    e.error()
                ));
            }
            MessageView::StreamStart(_) => {
                self.emit(PlayerEvent::Started(label_for(&self.current())));
            }
            _ => {}
        }
    }

    fn debug_stats(&self) {
        let st = self.0.sink.property::<gst::Structure>("stats");
        let get = |k: &str| st.get::<u64>(k).unwrap_or(0);
        let mut decoder = String::from("?");
        if let Some(bin) = self.0.playbin.downcast_ref::<gst::Bin>() {
            for el in bin.iterate_recurse().into_iter().flatten() {
                if let Some(f) = el.factory() {
                    if f.klass().contains("Decoder") && f.klass().contains("Video") {
                        decoder = f.name().to_string();
                    }
                }
            }
        }
        let caps = self
            .0
            .sink
            .static_pad("sink")
            .and_then(|p| p.current_caps())
            .and_then(|c| {
                c.structure(0).map(|s| {
                    s.get::<String>("drm-format")
                        .or_else(|_| s.get::<String>("format"))
                        .unwrap_or_default()
                })
            })
            .unwrap_or_default();
        eprintln!(
            "aloft: player decoder={decoder} format={caps} rendered={} dropped={}",
            get("rendered"),
            get("dropped")
        );
    }

    fn tick(&self) {
        let n = self.0.ticks.get() + 1;
        self.0.ticks.set(n);
        if crate::media::debug() && n.is_multiple_of(25) {
            self.debug_stats();
        }
        let pb = &self.0.playbin;
        let Some(pos) = pb.query_position::<gst::ClockTime>() else {
            return;
        };
        if pos > gst::ClockTime::from_seconds(1) {
            self.0.errors.set(0);
        }
        if !self.0.fades || self.0.faded.get() {
            return;
        }
        if let Some(dur) = pb.query_duration::<gst::ClockTime>() {
            let left = dur.saturating_sub(pos);
            if dur > gst::ClockTime::ZERO && left.mseconds() <= FADE.as_millis() as u64 {
                self.0.faded.set(true);
                self.emit(PlayerEvent::FadeOut);
            }
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let _ = self.playbin.set_state(gst::State::Null);
    }
}
