use crate::app::{Core, APP_ID};
use crate::config::Quality;
use crate::i18n::t;
use crate::idle::BackendKind;
use crate::library::{self, human_size};
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::Cell;
use std::rc::{Rc, Weak};
use std::time::Duration;

pub struct Window {
    pub window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
}

struct Live {
    core: Weak<Core>,
    have_row: adw::ActionRow,
    dl_row: adw::ActionRow,
    dl_button: gtk::Button,
    refresh_button: gtk::Button,
    progress: gtk::ProgressBar,
    lib_row: adw::ActionRow,
    lib_change: gtk::Button,
    quality_rows: Vec<(Quality, adw::ActionRow, gtk::Button)>,
    tick: Cell<u32>,
}

fn switch_row(title: &str, subtitle: &str, active: bool) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder()
        .title(title)
        .active(active)
        .build();
    if !subtitle.is_empty() {
        row.set_subtitle(subtitle);
    }
    row
}

fn spin_row(
    title: &str,
    subtitle: &str,
    min: f64,
    max: f64,
    step: f64,
    value: u32,
) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(min, max, step);
    row.set_title(title);
    row.set_subtitle(subtitle);
    row.set_value(value as f64);
    row
}

fn combo_row(title: &str, items: &[String], selected: u32) -> adw::ComboRow {
    let refs: Vec<&str> = items.iter().map(String::as_str).collect();
    let model = gtk::StringList::new(&refs);
    adw::ComboRow::builder()
        .title(title)
        .model(&model)
        .selected(selected)
        .build()
}

fn connect_switch(row: &adw::SwitchRow, core: &Rc<Core>, f: fn(&mut crate::config::Config, bool)) {
    let w = Rc::downgrade(core);
    row.connect_active_notify(move |r| {
        if let Some(c) = w.upgrade() {
            let v = r.is_active();
            c.update_config(|cfg| f(cfg, v));
        }
    });
}

fn connect_spin(row: &adw::SpinRow, core: &Rc<Core>, f: fn(&mut crate::config::Config, u32)) {
    let w = Rc::downgrade(core);
    row.connect_value_notify(move |r| {
        if let Some(c) = w.upgrade() {
            let v = r.value().round().max(0.0) as u32;
            c.update_config(|cfg| f(cfg, v));
        }
    });
}

impl Window {
    pub fn toast(&self, text: &str) {
        let toast = adw::Toast::new(text);
        toast.set_timeout(6);
        self.toasts.add_toast(toast);
    }

    pub fn new(core: &Rc<Core>) -> Window {
        let cfg = core.cfg.borrow().clone();
        let window = adw::ApplicationWindow::builder()
            .application(&core.app)
            .title("Aloft")
            .icon_name(APP_ID)
            .default_width(600)
            .default_height(760)
            .build();
        let toasts = adw::ToastOverlay::new();
        let page = adw::PreferencesPage::new();

        let saver = adw::PreferencesGroup::builder()
            .title(t("Screensaver", "Bildschirmschoner"))
            .build();

        if core.idle_backend() == BackendKind::None {
            let warn = adw::ActionRow::builder()
                    .title(t("Idle detection unavailable", "Leerlauf-Erkennung nicht verfügbar"))
                    .subtitle(t(
                        "Your desktop supports neither ext-idle-notify-v1 nor GNOME's idle monitor. You can still start it with \"aloft --start\", e.g. from swayidle.",
                        "Dein Desktop unterstützt weder ext-idle-notify-v1 noch GNOMEs Idle-Monitor. Starten geht trotzdem mit \"aloft --start\", z. B. aus swayidle.",
                    ))
                    .build();
            warn.add_prefix(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
            saver.add(&warn);
        }
        if let Some(e) = core.media_error.borrow().as_ref() {
            let warn = adw::ActionRow::builder()
                .title(t(
                    "Video playback unavailable",
                    "Videowiedergabe nicht verfügbar",
                ))
                .subtitle(e.as_str())
                .build();
            warn.add_prefix(&gtk::Image::from_icon_name("dialog-error-symbolic"));
            saver.add(&warn);
        }

        let enabled = switch_row(
            t("Start automatically", "Automatisch starten"),
            t(
                "When the computer has not been used for a while",
                "Wenn der Rechner eine Weile nicht benutzt wurde",
            ),
            cfg.enabled,
        );
        connect_switch(&enabled, core, |c, v| c.enabled = v);
        saver.add(&enabled);

        let idle = spin_row(
            t("Start after (minutes)", "Starten nach (Minuten)"),
            t(
                "Videos playing elsewhere hold it off",
                "Laufende Videos anderer Apps halten ihn zurück",
            ),
            1.0,
            240.0,
            1.0,
            cfg.idle_minutes,
        );
        connect_spin(&idle, core, |c, v| c.idle_minutes = v.max(1));
        enabled
            .bind_property("active", &idle, "sensitive")
            .sync_create()
            .build();
        saver.add(&idle);

        let stop = spin_row(
            t("Stop after (minutes)", "Beenden nach (Minuten)"),
            t(
                "0 = never. Afterwards the displays can go to sleep.",
                "0 = nie. Danach können die Monitore in den Ruhezustand.",
            ),
            0.0,
            1440.0,
            5.0,
            cfg.stop_after_minutes,
        );
        connect_spin(&stop, core, |c, v| c.stop_after_minutes = v);
        saver.add(&stop);

        let lock = switch_row(
            t(
                "Stop when the screen locks",
                "Beim Sperren des Bildschirms beenden",
            ),
            "",
            cfg.stop_on_lock,
        );
        connect_switch(&lock, core, |c, v| c.stop_on_lock = v);
        saver.add(&lock);

        let preview_row = adw::ActionRow::builder()
            .title(t("Preview", "Vorschau"))
            .subtitle(t(
                "Move the mouse or press a key to end it",
                "Maus bewegen oder Taste drücken zum Beenden",
            ))
            .build();
        let preview = gtk::Button::builder()
            .label(t("Start now", "Jetzt starten"))
            .valign(gtk::Align::Center)
            .action_name("app.preview")
            .build();
        preview.add_css_class("suggested-action");
        preview_row.add_suffix(&preview);
        preview_row.set_activatable_widget(Some(&preview));
        saver.add(&preview_row);
        page.add(&saver);

        let look = adw::PreferencesGroup::builder()
            .title(t("Appearance", "Darstellung"))
            .build();
        let monitors = combo_row(
            t("Multiple monitors", "Mehrere Monitore"),
            &[
                t("A different clip on each", "Eigener Clip auf jedem").to_string(),
                t("The same clip on all", "Derselbe Clip auf allen").to_string(),
            ],
            cfg.same_on_all as u32,
        );
        monitors.set_subtitle(t(
            "The same clip is decoded only once",
            "Derselbe Clip wird nur einmal dekodiert",
        ));
        {
            let w = Rc::downgrade(core);
            monitors.connect_selected_notify(move |r| {
                if let Some(c) = w.upgrade() {
                    let v = r.selected() == 1;
                    c.update_config(|cfg| cfg.same_on_all = v);
                }
            });
        }
        look.add(&monitors);
        let labels = switch_row(
            t("Show location names", "Ortsnamen einblenden"),
            "",
            cfg.show_labels,
        );
        connect_switch(&labels, core, |c, v| c.show_labels = v);
        look.add(&labels);
        let fades = switch_row(
            t("Fade between clips", "Zwischen Clips überblenden"),
            "",
            cfg.fades,
        );
        connect_switch(&fades, core, |c, v| c.fades = v);
        look.add(&fades);
        page.add(&look);

        let videos = adw::PreferencesGroup::builder()
            .title(t("Videos", "Videos"))
            .description(t(
                "Apple's aerial videos are downloaded straight from Apple's servers. They are not part of Aloft.",
                "Apples Aerial-Videos werden direkt von Apples Servern geladen. Sie sind nicht Teil von Aloft.",
            ))
            .build();
        let quality = combo_row(
            t("Quality", "Qualität"),
            &[
                "1080p".to_string(),
                t("1440p (converted from 4K)", "1440p (aus 4K umgerechnet)").to_string(),
                "4K".to_string(),
            ],
            cfg.quality.index(),
        );
        quality.set_subtitle(t(
            "1440p is converted on your GPU: about 30 % smaller than 4K, takes a few hours once",
            "1440p wird auf deiner GPU umgerechnet: etwa 30 % kleiner als 4K, dauert einmalig einige Stunden",
        ));
        videos.add(&quality);

        let lib_row = adw::ActionRow::builder()
            .title(t("Location", "Speicherort"))
            .subtitle(cfg.library_dir().to_string_lossy().as_ref())
            .subtitle_selectable(true)
            .build();
        let open = gtk::Button::from_icon_name("folder-open-symbolic");
        open.set_tooltip_text(Some(t("Open folder", "Ordner öffnen")));
        open.set_valign(gtk::Align::Center);
        open.add_css_class("flat");
        let lib_change = gtk::Button::builder()
            .label(t("Change…", "Ändern…"))
            .valign(gtk::Align::Center)
            .build();
        lib_row.add_suffix(&open);
        lib_row.add_suffix(&lib_change);
        videos.add(&lib_row);

        let have_row = adw::ActionRow::builder()
            .title(t("Available", "Vorhanden"))
            .build();
        videos.add(&have_row);

        let dl_row = adw::ActionRow::builder()
            .title(t("Download", "Herunterladen"))
            .build();
        let refresh_button = gtk::Button::from_icon_name("view-refresh-symbolic");
        refresh_button.set_tooltip_text(Some(t(
            "Check Apple for new clips and fetch them",
            "Bei Apple nach neuen Clips sehen und sie laden",
        )));
        refresh_button.set_valign(gtk::Align::Center);
        refresh_button.add_css_class("flat");
        let dl_button = gtk::Button::builder().valign(gtk::Align::Center).build();
        dl_row.add_suffix(&refresh_button);
        dl_row.add_suffix(&dl_button);
        videos.add(&dl_row);
        let progress = gtk::ProgressBar::builder()
            .margin_top(10)
            .show_text(true)
            .visible(false)
            .build();
        videos.add(&progress);
        page.add(&videos);

        let storage = adw::PreferencesGroup::builder()
            .title(t("Storage", "Speicherplatz"))
            .build();
        let mut quality_rows = Vec::new();
        for q in Quality::ALL {
            let row = adw::ActionRow::builder().title(q.dir_name()).build();
            let del = gtk::Button::from_icon_name("user-trash-symbolic");
            del.set_tooltip_text(Some(t("Delete these videos", "Diese Videos löschen")));
            del.set_valign(gtk::Align::Center);
            del.add_css_class("flat");
            row.add_suffix(&del);
            storage.add(&row);
            quality_rows.push((q, row, del));
        }
        page.add(&storage);

        let system = adw::PreferencesGroup::builder()
            .title(t("System", "System"))
            .build();
        let gpus = crate::gpu::list();
        if gpus.len() > 1 {
            let mut items = vec![t("Automatic", "Automatisch").to_string()];
            items.extend(gpus.iter().map(|g| g.label()));
            let selected = gpus
                .iter()
                .position(|g| g.id == cfg.gpu)
                .map_or(0, |i| i as u32 + 1);
            let gpu = combo_row(
                t("Decode videos on", "Videos dekodieren auf"),
                &items,
                selected,
            );
            let gpus = Rc::new(gpus);
            let chosen = {
                let gpus = gpus.clone();
                move |i: u32| match i {
                    0 => gpus.iter().find(|g| g.integrated).cloned(),
                    i => gpus.get(i as usize - 1).cloned(),
                }
            };
            gpu.set_subtitle(gpu_hint(chosen(selected).as_ref()));
            let w = Rc::downgrade(core);
            gpu.connect_selected_notify(move |r| {
                let g = chosen(r.selected());
                r.set_subtitle(gpu_hint(g.as_ref()));
                let Some(c) = w.upgrade() else { return };
                let id = if r.selected() == 0 {
                    "auto".to_string()
                } else {
                    g.map_or("auto".into(), |g| g.id)
                };
                c.update_config(|cfg| cfg.gpu = id);
                c.restart_service();
            });
            system.add(&gpu);
        }

        let autostart = switch_row(
            t(
                "Run in the background after login",
                "Nach der Anmeldung im Hintergrund starten",
            ),
            t(
                "Needed for the automatic start",
                "Nötig für den automatischen Start",
            ),
            cfg.autostart,
        );
        connect_switch(&autostart, core, |c, v| c.autostart = v);
        system.add(&autostart);
        page.add(&system);

        let menu = gio::Menu::new();
        menu.append(Some(t("About Aloft", "Über Aloft")), Some("win.about"));
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .primary(true)
            .build();
        let header = adw::HeaderBar::new();
        header.pack_end(&menu_button);
        let about = gio::SimpleAction::new("about", None);
        {
            let win = window.downgrade();
            about.connect_activate(move |_, _| {
                if let Some(win) = win.upgrade() {
                    about_dialog().present(Some(&win));
                }
            });
        }
        window.add_action(&about);

        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        toasts.set_child(Some(&page));
        view.set_content(Some(&toasts));
        window.set_content(Some(&view));

        let live = Rc::new(Live {
            core: Rc::downgrade(core),
            have_row,
            dl_row,
            dl_button: dl_button.clone(),
            refresh_button: refresh_button.clone(),
            progress,
            lib_row,
            lib_change: lib_change.clone(),
            quality_rows,
            tick: Cell::new(0),
        });

        {
            let l = Rc::downgrade(&live);
            quality.connect_selected_notify(move |r| {
                let Some(l) = l.upgrade() else { return };
                let Some(c) = l.core.upgrade() else { return };
                let q = Quality::from_index(r.selected());
                c.update_config(|cfg| cfg.quality = q);
                l.refresh(true);
            });
        }
        {
            let w = Rc::downgrade(core);
            let l = Rc::downgrade(&live);
            dl_button.connect_clicked(move |_| {
                let Some(c) = w.upgrade() else { return };
                if c.dl.borrow().running {
                    c.cancel_download();
                } else {
                    c.start_download(false);
                }
                if let Some(l) = l.upgrade() {
                    l.refresh(false);
                }
            });
        }
        {
            let w = Rc::downgrade(core);
            let l = Rc::downgrade(&live);
            refresh_button.connect_clicked(move |_| {
                let Some(c) = w.upgrade() else { return };
                c.start_download(true);
                if let Some(l) = l.upgrade() {
                    l.refresh(false);
                }
            });
        }
        {
            let w = Rc::downgrade(core);
            open.connect_clicked(move |b| {
                let Some(c) = w.upgrade() else { return };
                let dir = c.cfg.borrow().library_dir();
                let _ = std::fs::create_dir_all(&dir);
                let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&dir)));
                let root = b.root().and_downcast::<gtk::Window>();
                launcher.launch(root.as_ref(), gio::Cancellable::NONE, |_| {});
            });
        }
        {
            let w = Rc::downgrade(core);
            let l = Rc::downgrade(&live);
            lib_change.connect_clicked(move |b| {
                let Some(c) = w.upgrade() else { return };
                let dialog = gtk::FileDialog::builder()
                    .title(t("Choose video folder", "Video-Ordner wählen"))
                    .modal(true)
                    .initial_folder(&gio::File::for_path(c.cfg.borrow().library_dir()))
                    .build();
                let root = b.root().and_downcast::<gtk::Window>();
                let w = w.clone();
                let l = l.clone();
                dialog.select_folder(root.as_ref(), gio::Cancellable::NONE, move |res| {
                    let (Ok(folder), Some(c)) = (res, w.upgrade()) else {
                        return;
                    };
                    let Some(path) = folder.path() else { return };
                    c.update_config(|cfg| cfg.library_dir = Some(path));
                    if let Some(l) = l.upgrade() {
                        l.refresh(true);
                    }
                });
            });
        }
        for (q, _, del) in &live.quality_rows {
            let q = *q;
            let w = Rc::downgrade(core);
            let l = Rc::downgrade(&live);
            del.connect_clicked(move |b| {
                let Some(c) = w.upgrade() else { return };
                let dir = library::quality_dir(&c.cfg.borrow().library_dir(), q);
                let (n, bytes) = library::stats(&dir);
                let heading =
                    t("Delete {} videos?", "{}-Videos löschen?").replace("{}", q.dir_name());
                let body = t(
                    "{n} clips ({size}) will be deleted from disk.",
                    "{n} Clips ({size}) werden von der Platte gelöscht.",
                )
                .replace("{n}", &n.to_string())
                .replace("{size}", &human_size(bytes));
                let dialog = adw::AlertDialog::new(Some(&heading), Some(&body));
                dialog.add_responses(&[
                    ("cancel", t("Cancel", "Abbrechen")),
                    ("delete", t("Delete", "Löschen")),
                ]);
                dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");
                let l = l.clone();
                dialog.connect_response(None, move |_, resp| {
                    if resp != "delete" {
                        return;
                    }
                    for f in library::videos_in(&dir) {
                        let _ = std::fs::remove_file(f);
                    }
                    if let Some(l) = l.upgrade() {
                        l.refresh(true);
                    }
                });
                if let Some(root) = b.root().and_downcast::<gtk::Window>() {
                    dialog.present(Some(&root));
                }
            });
        }

        live.refresh(true);
        let l = Rc::downgrade(&live);
        let keep = live.clone();
        glib::timeout_add_local(Duration::from_millis(500), move || {
            let _ = &keep;
            match l.upgrade() {
                Some(l) if l.dl_row.root().is_some() => {
                    l.refresh(false);
                    glib::ControlFlow::Continue
                }
                _ => glib::ControlFlow::Break,
            }
        });

        Window { window, toasts }
    }
}

impl Live {
    fn refresh(&self, force_stats: bool) {
        let Some(c) = self.core.upgrade() else { return };
        c.poll_download();
        let cfg = c.cfg.borrow().clone();
        let st = c.dl.borrow().clone();

        let tick = self.tick.get().wrapping_add(1);
        self.tick.set(tick);
        if force_stats || tick.is_multiple_of(4) {
            let lib = cfg.library_dir();
            self.lib_row.set_subtitle(&lib.to_string_lossy());
            let (n, bytes) = library::stats(&library::quality_dir(&lib, cfg.quality));
            let total = library::load_catalog(&library::catalog_path(&cfg))
                .map(|a| a.iter().filter(|a| a.url(cfg.quality).is_some()).count())
                .unwrap_or(0);
            let have = if total > 0 {
                format!(
                    "{n} {} {total} {} · {}",
                    t("of", "von"),
                    t("clips", "Clips"),
                    human_size(bytes)
                )
            } else {
                format!("{n} {} · {}", t("clips", "Clips"), human_size(bytes))
            };
            self.have_row
                .set_subtitle(&format!("{}: {have}", cfg.quality.dir_name()));
            for (q, row, del) in &self.quality_rows {
                let (n, bytes) = library::stats(&library::quality_dir(&lib, *q));
                row.set_subtitle(&format!(
                    "{n} {} · {}",
                    t("clips", "Clips"),
                    human_size(bytes)
                ));
                del.set_sensitive(n > 0 && !(st.running && *q == cfg.quality));
            }
        }

        self.lib_change.set_sensitive(!st.running);
        self.refresh_button.set_sensitive(!st.running);
        if st.running {
            self.dl_button.set_label(t("Cancel", "Abbrechen"));
            self.dl_button.remove_css_class("suggested-action");
            let mut status = st.phase.clone();
            if st.todo > 0 {
                status = format!(
                    "{} {} {} · {}/s",
                    st.done,
                    t("of", "von"),
                    st.todo,
                    human_size(st.rate as u64)
                );
                if st.failed > 0 {
                    status.push_str(&format!(
                        " · {} {}",
                        st.failed,
                        t("failed", "fehlgeschlagen")
                    ));
                }
            }
            self.dl_row.set_subtitle(&status);
            self.progress.set_visible(true);
            if st.todo > 0 {
                self.progress.set_fraction(st.done as f64 / st.todo as f64);
                self.progress.set_text(Some(&format!(
                    "{} {}",
                    human_size(st.bytes),
                    t("loaded", "geladen")
                )));
            } else {
                self.progress.pulse();
                self.progress.set_text(Some(&st.phase));
            }
        } else {
            self.dl_button.set_label(t("Download", "Herunterladen"));
            self.dl_button.add_css_class("suggested-action");
            self.progress.set_visible(false);
            let sub = st.result.clone().unwrap_or_else(|| {
                t(
                    "Fetches all missing clips in the chosen quality",
                    "Lädt alle fehlenden Clips in der gewählten Qualität",
                )
                .to_string()
            });
            self.dl_row.set_subtitle(&sub);
        }
    }
}

fn gpu_hint(gpu: Option<&crate::gpu::Gpu>) -> &'static str {
    match gpu {
        Some(g) if g.integrated => t(
            "Leaves the dedicated GPU free. The picture is still drawn by the GPU your desktop runs on.",
            "Die dedizierte Grafikkarte bleibt frei. Gezeichnet wird weiterhin auf der GPU, auf der dein Desktop läuft.",
        ),
        Some(_) => t(
            "Uses video memory of this GPU while the screensaver runs.",
            "Belegt Videospeicher dieser Grafikkarte, solange der Bildschirmschoner läuft.",
        ),
        None => t("GStreamer picks the decoder.", "GStreamer wählt den Decoder."),
    }
}

fn about_dialog() -> adw::AboutDialog {
    adw::AboutDialog::builder()
        .application_name("Aloft")
        .application_icon(APP_ID)
        .version(env!("CARGO_PKG_VERSION"))
        .developer_name("Eric S.")
        .license_type(gtk::License::Custom)
        .license(t(
            "Creative Commons BY-NC-SA 4.0: you may use, share and change Aloft, but not sell it, and changed versions have to stay under the same license.",
            "Creative Commons BY-NC-SA 4.0: Du darfst Aloft nutzen, weitergeben und verändern, aber nicht verkaufen, und veränderte Versionen müssen unter derselben Lizenz bleiben.",
        ))
        .website("https://creativecommons.org/licenses/by-nc-sa/4.0/")
        .comments(t(
            "Aerial screensaver for Wayland. The videos come straight from Apple's servers and are not part of Aloft. Aloft is not affiliated with Apple.",
            "Aerial-Bildschirmschoner für Wayland. Die Videos kommen direkt von Apples Servern und sind nicht Teil von Aloft. Aloft hat nichts mit Apple zu tun.",
        ))
        .build()
}
