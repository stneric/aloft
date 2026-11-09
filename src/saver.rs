use crate::config::Config;
use crate::player::{Player, PlayerEvent};
use gtk::prelude::*;
use gtk::{gdk, glib, pango};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

pub const CSS: &str = r#"
window.aloft-saver, window.aloft-saver > * { background: black; }
.aloft-video { opacity: 1; }
.aloft-video.fades { transition: opacity 1500ms ease-in-out; }
.aloft-video.faded { opacity: 0; }
.aloft-label {
  color: white;
  font-weight: 300;
  text-shadow: 0 2px 8px alpha(black, 0.6);
  opacity: 0;
  transition: opacity 1200ms ease-in-out;
}
.aloft-label.shown { opacity: 1; }
"#;

const LABEL_SECONDS: u64 = 7;
const MOTION_THRESHOLD: f64 = 30.0;

struct View {
    picture: gtk::Picture,
    label: gtk::Label,
    label_gen: Cell<u64>,
}

pub struct Session {
    windows: Vec<gtk::Window>,
    players: Vec<Player>,
    stop_timer: Rc<RefCell<Option<glib::SourceId>>>,
}

impl Session {
    pub fn start(
        app: &impl IsA<gtk::Application>,
        cfg: &Config,
        files: Vec<PathBuf>,
        manual: bool,
        render: Option<&crate::gpu::Gpu>,
        on_end: Rc<dyn Fn(Option<String>)>,
    ) -> Result<Session, String> {
        let display = gdk::Display::default().ok_or("no display")?;
        let monitors: Vec<gdk::Monitor> = display
            .monitors()
            .iter::<gdk::Monitor>()
            .filter_map(Result::ok)
            .collect();
        if monitors.is_empty() {
            return Err("no monitors".into());
        }

        let ended = Rc::new(Cell::new(false));
        let end: Rc<dyn Fn(Option<String>)> = {
            let ended = ended.clone();
            Rc::new(move |err| {
                if ended.replace(true) {
                    return;
                }
                if crate::media::debug() {
                    eprintln!(
                        "aloft: screensaver ends ({})",
                        err.as_deref().unwrap_or("input/timer")
                    );
                }
                let on_end = on_end.clone();
                glib::idle_add_local_once(move || on_end(err));
            })
        };

        let shared = cfg.same_on_all || monitors.len() == 1;
        let mut players = Vec::new();
        let mut views: Vec<Vec<Rc<View>>> = Vec::new();
        if shared {
            players.push(Player::new(files.clone(), cfg.fades, render)?);
            views.push(Vec::new());
        }

        let started = Instant::now();
        let grace = if manual {
            Duration::from_millis(1200)
        } else {
            Duration::from_millis(400)
        };
        let mut windows = Vec::new();
        for (i, monitor) in monitors.iter().enumerate() {
            let player_idx = if shared {
                0
            } else {
                players.push(Player::new(files.clone(), cfg.fades, render)?);
                views.push(Vec::new());
                i
            };
            let player = &players[player_idx];

            let picture = gtk::Picture::for_paintable(&player.paintable());
            picture.set_content_fit(gtk::ContentFit::Cover);
            picture.set_can_shrink(true);
            picture.add_css_class("aloft-video");
            if cfg.fades {
                picture.add_css_class("fades");
                picture.add_css_class("faded");
            }

            let height = monitor.geometry().height().max(480) as f64;
            let label = gtk::Label::new(None);
            label.add_css_class("aloft-label");
            label.set_halign(gtk::Align::Start);
            label.set_valign(gtk::Align::End);
            let margin = (height / 22.0) as i32;
            label.set_margin_start(margin);
            label.set_margin_bottom(margin);
            let attrs = pango::AttrList::new();
            attrs.insert(pango::AttrSize::new_size_absolute(
                (height / 30.0) as i32 * pango::SCALE,
            ));
            label.set_attributes(Some(&attrs));
            label.set_visible(cfg.show_labels);

            let overlay = gtk::Overlay::new();
            overlay.set_child(Some(&picture));
            overlay.add_overlay(&label);

            let window = gtk::Window::builder()
                .application(app.upcast_ref::<gtk::Application>())
                .title("Aloft")
                .decorated(false)
                .child(&overlay)
                .build();
            window.add_css_class("aloft-saver");
            window.set_cursor_from_name(Some("none"));
            add_input_handlers(&window, end.clone(), started, grace);
            {
                let end = end.clone();
                window.connect_close_request(move |_| {
                    end(None);
                    glib::Propagation::Stop
                });
            }
            window.fullscreen_on_monitor(monitor);
            window.present();
            windows.push(window);
            views[player_idx].push(Rc::new(View {
                picture,
                label,
                label_gen: Cell::new(0),
            }));
        }

        for (player, views) in players.iter().zip(views) {
            let end = end.clone();
            let show_labels = cfg.show_labels;
            player.connect(move |ev| match ev {
                PlayerEvent::Started(text) => {
                    for v in &views {
                        v.picture.remove_css_class("faded");
                        if show_labels {
                            show_label(v, &text);
                        }
                    }
                }
                PlayerEvent::FadeOut => {
                    for v in &views {
                        v.picture.add_css_class("faded");
                    }
                }
                PlayerEvent::Failed(err) => end(Some(err)),
            });
            player.start();
        }

        let stop_timer: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
        if cfg.stop_after_minutes > 0 {
            let end = end.clone();
            let slot = stop_timer.clone();
            let id = glib::timeout_add_seconds_local_once(cfg.stop_after_minutes * 60, move || {
                slot.take();
                end(None)
            });
            stop_timer.replace(Some(id));
        }

        if crate::media::debug() {
            let ws: Vec<glib::WeakRef<gtk::Window>> =
                windows.iter().map(|w| w.downgrade()).collect();
            glib::timeout_add_seconds_local(5, move || {
                let mut alive = false;
                for (i, w) in ws.iter().enumerate() {
                    if let Some(w) = w.upgrade() {
                        alive = true;
                        if let Some(fc) = w.frame_clock() {
                            eprintln!("aloft: window {i} fps={:.1}", fc.fps());
                        }
                    }
                }
                if alive {
                    glib::ControlFlow::Continue
                } else {
                    glib::ControlFlow::Break
                }
            });
        }

        Ok(Session {
            windows,
            players,
            stop_timer,
        })
    }
}

fn show_label(v: &Rc<View>, text: &str) {
    v.label.set_text(text);
    v.label.add_css_class("shown");
    let gen = v.label_gen.get() + 1;
    v.label_gen.set(gen);
    let v = Rc::downgrade(v);
    glib::timeout_add_seconds_local_once(LABEL_SECONDS as u32, move || {
        if let Some(v) = v.upgrade() {
            if v.label_gen.get() == gen {
                v.label.remove_css_class("shown");
            }
        }
    });
}

fn add_input_handlers(
    window: &gtk::Window,
    end: Rc<dyn Fn(Option<String>)>,
    started: Instant,
    grace: Duration,
) {
    let key = gtk::EventControllerKey::new();
    {
        let end = end.clone();
        key.connect_key_pressed(move |_, _, _, _| {
            if crate::media::debug() {
                eprintln!("aloft: key");
            }
            if started.elapsed() > grace {
                end(None);
            }
            glib::Propagation::Stop
        });
    }
    window.add_controller(key);

    let click = gtk::GestureClick::new();
    click.set_button(0);
    {
        let end = end.clone();
        click.connect_pressed(move |_, _, _, _| {
            if crate::media::debug() {
                eprintln!("aloft: click");
            }
            if started.elapsed() > grace {
                end(None);
            }
        });
    }
    window.add_controller(click);

    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
    {
        let end = end.clone();
        scroll.connect_scroll(move |_, _, _| {
            if started.elapsed() > grace {
                end(None);
            }
            glib::Propagation::Stop
        });
    }
    window.add_controller(scroll);

    // Windows appearing under the pointer get a synthetic motion; only real
    // travel after the grace period ends the screensaver.
    let motion = gtk::EventControllerMotion::new();
    let origin: Rc<RefCell<Option<(f64, f64)>>> = Rc::new(RefCell::new(None));
    motion.connect_motion(move |_, x, y| {
        let mut o = origin.borrow_mut();
        if started.elapsed() <= grace {
            *o = Some((x, y));
            return;
        }
        match *o {
            None => *o = Some((x, y)),
            Some((ox, oy)) => {
                if (x - ox).hypot(y - oy) > MOTION_THRESHOLD {
                    if crate::media::debug() {
                        eprintln!("aloft: pointer moved");
                    }
                    end(None);
                }
            }
        }
    });
    window.add_controller(motion);
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(t) = self.stop_timer.take() {
            t.remove();
        }
        for p in &self.players {
            p.stop();
        }
        for w in &self.windows {
            w.destroy();
        }
    }
}
