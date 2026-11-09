mod app;
mod config;
mod download;
mod gpu;
mod i18n;
mod idle;
mod library;
mod media;
mod player;
mod saver;
mod service;
mod settings;
mod system;
mod transcode;

const HELP: &str = "\
Usage: aloft [OPTION]

  (none)          open the settings (starts the background service if needed)
  --background    run the background service (used for autostart)
  --start         start the screensaver now
  --stop          stop the screensaver
  --quit          stop the background service
  --download      download missing clips without a window
      --quality 1080p|1440p|4k   (default: the configured quality)
      --refresh                  check Apple for new clips first
  --version       print the version
  --help          show this help
";

fn main() -> gtk::glib::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return gtk::glib::ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("aloft {}", env!("CARGO_PKG_VERSION"));
        return gtk::glib::ExitCode::SUCCESS;
    }

    let cfg = config::Config::load();
    let render = gpu::select(&cfg.gpu);
    if args.iter().any(|a| a == "--download") {
        return cli_download(&args, cfg, render);
    }
    gtk::glib::set_application_name("Aloft");
    const SERVICE_ARGS: [&str; 6] = ["--background", "-b", "--start", "--now", "--stop", "--quit"];
    if args.iter().any(|a| SERVICE_ARGS.contains(&a.as_str())) {
        // Only decoding moves to `render`. Presenting from a second GPU stutters,
        // so frames are drawn by the desktop's GPU, and its GL renderer imports
        // linear dmabufs from another GPU most reliably.
        if std::env::var_os("GSK_RENDERER").is_none() {
            std::env::set_var("GSK_RENDERER", "gl");
        }
        return service::run(render);
    }
    app::run(render)
}

fn cli_download(
    args: &[String],
    cfg: config::Config,
    render: Option<gpu::Gpu>,
) -> gtk::glib::ExitCode {
    use std::sync::atomic::Ordering;
    let mut quality = cfg.quality;
    if let Some(i) = args.iter().position(|a| a == "--quality") {
        quality = match args.get(i + 1).map(|s| s.to_ascii_lowercase()).as_deref() {
            Some("1080p") | Some("1080") => config::Quality::Hd1080,
            Some("1440p") | Some("1440") => config::Quality::Qhd1440,
            Some("4k") | Some("2160p") => config::Quality::Uhd4k,
            other => {
                eprintln!("unknown quality {other:?} (1080p, 1440p, 4k)");
                return gtk::glib::ExitCode::FAILURE;
            }
        };
    }
    if let Err(e) = media::init(render.as_ref()) {
        eprintln!("GStreamer: {e}");
        return gtk::glib::ExitCode::FAILURE;
    }
    let lib = cfg.library_dir();
    println!(
        "{} → {}",
        quality.dir_name(),
        library::quality_dir(&lib, quality).display()
    );
    let handle = download::start(download::Params {
        lib,
        quality,
        refresh_catalog: args.iter().any(|a| a == "--refresh"),
        jobs: cfg.download_jobs as usize,
        render,
    });
    let bytes = handle.bytes.clone();
    std::thread::spawn(move || {
        let mut last = 0;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(10));
            let now = bytes.load(Ordering::Relaxed);
            println!(
                "   {} ({}/s)",
                library::human_size(now),
                library::human_size((now - last) / 10)
            );
            last = now;
        }
    });
    let (mut todo, mut done, mut failed) = (0, 0, 0);
    while let Ok(msg) = handle.rx.recv_blocking() {
        match msg {
            download::Msg::Phase(p) => println!(":: {p}"),
            download::Msg::Planned { todo: t, catalog } => {
                todo = t;
                println!(":: {catalog} clips in the catalog, {t} missing");
            }
            download::Msg::ClipDone { name, error } => {
                done += 1;
                match error {
                    None => println!("  ok  [{done}/{todo}] {name}"),
                    Some(e) => {
                        failed += 1;
                        println!("  !!  [{done}/{todo}] {name}: {e}");
                    }
                }
            }
            download::Msg::Finished { cancelled, error } => {
                if let Some(e) = error {
                    eprintln!(":: {e}");
                    return gtk::glib::ExitCode::FAILURE;
                }
                if cancelled {
                    return gtk::glib::ExitCode::FAILURE;
                }
                println!(":: done, {failed} failed");
                return if failed > 0 {
                    gtk::glib::ExitCode::FAILURE
                } else {
                    gtk::glib::ExitCode::SUCCESS
                };
            }
        }
    }
    gtk::glib::ExitCode::FAILURE
}
