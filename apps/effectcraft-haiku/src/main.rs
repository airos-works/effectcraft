//! EffectCraft hosted by the native Be API on air/OS: a `BApplication` and a `BWindow` per egui
//! viewport (`native/host.cpp`, `viewports`, `platform`), drawn with Vulkan on the device the
//! compositor, effects and exports share (`gpu`, `present`) or by a software rasteriser
//! (`raster`), audio through the Media Kit (`audio`).
//!
//! Usage: `EffectCraft [--control <port>] [--demo|--empty|--home] [--] [project.ecproj | media files…]`,
//! as upstream's desktop app (`apps/effectcraft/src/main.rs`).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![cfg_attr(not(target_os = "haiku"), allow(dead_code))]

#[cfg(target_os = "haiku")]
mod audio;
#[cfg(target_os = "haiku")]
#[path = "../../effectcraft/src/control_server.rs"]
mod control_server;
mod damage;
#[cfg(target_os = "haiku")]
mod gpu;
mod guard;
mod health;
#[cfg(target_os = "haiku")]
mod host;
mod keys;
#[cfg(target_os = "haiku")]
mod media_kit;
mod native_menu;
#[cfg(target_os = "haiku")]
mod platform;
#[cfg(target_os = "haiku")]
mod present;
mod raster;
#[cfg(target_os = "haiku")]
mod scanout;
#[cfg(target_os = "haiku")]
mod services;
#[cfg(target_os = "haiku")]
mod shell;
mod viewports;

/// The command line.
#[derive(Debug, Default, PartialEq)]
struct Args {
    control_port: Option<u16>,
    files: Vec<String>,
    /// Open the demo project when no project is given (`--demo`).
    demo: bool,
    /// Show or skip the Home screen (`--home`), else the Startup preference decides.
    home: Option<bool>,
}

/// A control-channel port from 1 to 65535; `name` is the flag or variable it came from.
fn control_port(name: &str, value: Option<&str>) -> Result<u16, String> {
    let need = format!("{name} needs a port number from 1 to 65535");
    match value {
        None => Err(need),
        Some(v) => v.parse().ok().filter(|&p| p != 0).ok_or(format!("{need}, not `{v}`")),
    }
}

/// The arguments after the program name, as upstream's desktop app reads them; `Ok(None)` for
/// `--version`. `env_port` is `EFFECTCRAFT_CONTROL_PORT`, used when `--control` isn't given.
fn parse_args(args: impl IntoIterator<Item = String>, env_port: Option<String>) -> Result<Option<Args>, String> {
    let mut out = Args::default();
    let mut args = args.into_iter();
    let mut options = true;
    while let Some(a) = args.next() {
        if !options {
            out.files.push(a);
            continue;
        }
        match a.as_str() {
            "--" => options = false,
            "--control" => out.control_port = Some(control_port("--control", args.next().as_deref())?),
            "--demo" => out.demo = true,
            "--empty" => out.demo = false,
            "--home" => out.home = Some(true),
            "--version" => return Ok(None),
            _ => match a.strip_prefix("--control=") {
                Some(v) => out.control_port = Some(control_port("--control", Some(v))?),
                None if a.starts_with('-') && a != "-" => return Err(format!("unknown option `{a}`")),
                None => out.files.push(a),
            },
        }
    }
    if out.control_port.is_none()
        && let Some(v) = env_port.filter(|v| !v.is_empty())
    {
        out.control_port = Some(control_port("EFFECTCRAFT_CONTROL_PORT", Some(&v))?);
    }
    Ok(Some(out))
}

/// Projects (`.ecproj`, `.ecprojx`) open; other files are imported.
fn is_project_file(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("ecproj") || ext.eq_ignore_ascii_case("ecprojx"))
}

/// The worker threads heavy work (rendering, tracking, SAM 2, encoders) shares: air/OS gets
/// slower past about four threads for this kind of work (6.8 s with 4 against 18 s with 32 on a
/// 32-thread Threadripper, measured with LightCraft's SAM), so at most four unless
/// `EFFECTCRAFT_THREADS` says otherwise.
fn worker_threads(cpus: usize, env: Option<&str>) -> usize {
    env.and_then(|v| v.trim().parse::<usize>().ok()).filter(|n| (1..=256).contains(n)).unwrap_or_else(|| cpus.clamp(1, 4))
}

#[cfg(not(target_os = "haiku"))]
fn main() {
    eprintln!("EffectCraft for air/OS requires Haiku; run `effectcraft` on other platforms.");
    std::process::exit(1);
}

#[cfg(target_os = "haiku")]
fn main() {
    if let Err(e) = native_main() {
        eprintln!("EffectCraft: {e}");
        std::process::exit(1);
    }
}

#[cfg(target_os = "haiku")]
fn native_main() -> Result<(), String> {
    use effectcraft_ui_egui::EffectcraftApp;
    use serde_json::json;
    host::export_locale();
    effectcraft_engine::logging::install();
    effectcraft_engine::logging::install_panic_hook();
    let Some(Args { control_port, files, demo, home }) = parse_args(std::env::args().skip(1), std::env::var("EFFECTCRAFT_CONTROL_PORT").ok())? else {
        let sha = option_env!("EFFECTCRAFT_BUILD_SHA").unwrap_or("");
        println!("EffectCraft {} for air/OS{}", env!("CARGO_PKG_VERSION"), if sha.is_empty() { String::new() } else { format!(" ({})", sha.get(..12).unwrap_or(sha)) });
        return Ok(());
    };
    // Before the log file, the preferences or the recovery records are touched: a second launch
    // (from Terminal; Tracker hands documents to the running copy itself) passes its documents to
    // the running copy and ends. An agent's launch (--control) needs a process of its own.
    if host::init(services::SIGNATURE)? == host::Launch::HandedOver {
        if control_port.is_some() {
            return Err("EffectCraft is already running from this file; --control needs a copy of its own (run it from another file)".into());
        }
        std::process::exit(0);
    }
    if let Some(dir) = effectcraft_host::config_dir() {
        match effectcraft_engine::logging::attach_session_log(&effectcraft_engine::logging::log_dir(Some(&dir))) {
            Ok(path) => log::info!("EffectCraft {} for air/OS, log file {}", env!("CARGO_PKG_VERSION"), path.display()),
            Err(e) => log::warn!("no log file: {e}"),
        }
    }
    effectcraft_engine::text::fonts::scan_system_in_background();
    services::claim_document_types();
    let cpus = std::thread::available_parallelism().map_or(4, |n| n.get());
    let workers = worker_threads(cpus, std::env::var("EFFECTCRAFT_THREADS").ok().as_deref());
    if let Err(e) = rayon::ThreadPoolBuilder::new().num_threads(workers).thread_name(|i| format!("ec-worker-{i}")).build_global() {
        log::warn!("worker pool: {e}");
    }
    log::info!("{workers} worker threads ({cpus} processors)");
    // Bound before the window opens, as the desktop app does: a port in use ends the launch.
    if let Some(port) = control_port {
        std::net::TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("control server failed to bind 127.0.0.1:{port}: {e}"))?;
    }
    let scale = host::ui_scale();
    // The main window opens before the interface starts, so it shows at once. Upstream's default
    // window (1680 × 1020 points) where the monitor has room, else most of it.
    let root = host::open_window(&host::WindowSpec {
        title: "EffectCraft".into(),
        position: None,
        size: [1680.0 * scale, 1020.0 * scale],
        min_size: Some([960.0 * scale, 600.0 * scale]),
        max_size: None,
        decorations: true,
        resizable: true,
        visible: true,
        zoomed: false,
        fullscreen: false,
        floating: false,
        buttons: [true; 3],
    })
    .ok_or("couldn't open the window")?;
    let worker = std::thread::Builder::new()
        .name("EffectCraft interface".into())
        .stack_size(32 << 20)
        .spawn(move || {
            let result = guard::guard("Interface", || {
                let ctx = egui::Context::default();
                EffectcraftApp::install_ime_reset_hooks(&ctx);
                let mut session = effectcraft_host::session();
                // Render Queue outputs get their file types.
                if let Some(exporter) = session.exporter.take() {
                    session.exporter = Some(std::sync::Arc::new(services::TypedExporter(exporter)));
                }
                // Movies and sounds the app's own decoders can't read come from the Media Kit.
                let kit = media_kit::Use::from_env(std::env::var("EFFECTCRAFT_MEDIA_KIT").ok().as_deref());
                if kit != media_kit::Use::Off {
                    session.footage = std::sync::Arc::new(media_kit::KitFootage::new(session.footage.clone()));
                    if let Some(importer) = session.importer.take() {
                        session.importer = Some(std::sync::Arc::new(media_kit::KitImporter { inner: importer, mode: kit }));
                    }
                }
                session.check_footage_on_open = true;
                session.autosave.background = true;
                session.disk_salt_background = true;
                if let Some(dir) = effectcraft_host::config_dir() {
                    let plugins = dir.join(effectcraft_engine::commands::PLUGINS_DIR);
                    if plugins.is_dir() {
                        let _ = session.execute("effect.plugins.load", json!({"folder": plugins.to_string_lossy()}));
                        if let Err(e) = session.execute("library.loadInstalled", json!({})) {
                            log::warn!("plug-in library: {e}");
                        }
                    }
                    // External effect helpers are native programs: only when asked
                    // (`EFFECTCRAFT_HELPERS=1`, or Effect ▸ Scan for Effect Helpers).
                    if session.helpers.is_some() && std::env::var_os("EFFECTCRAFT_HELPERS").is_some_and(|v| v == "1") {
                        match session.execute("effect.helpers.scan", json!({})) {
                            Ok(r) => {
                                for e in r["errors"].as_array().into_iter().flatten() {
                                    log::warn!("effect helpers: {}", e.as_str().unwrap_or("error"));
                                }
                            }
                            Err(e) => log::warn!("effect helpers: {e}"),
                        }
                    }
                    session.config = Some(std::sync::Arc::new(effectcraft_engine::config::DirConfig::new(dir)));
                }
                session.load_settings();
                let renderer = shell::Renderer::choose(std::env::var("EFFECTCRAFT_RENDERER").ok().as_deref(), &session.prefs.startup.window_graphics)?;
                let _ = session.execute("file.runStartupScripts", json!({}));
                let recovery = if control_port.is_none() && std::env::var_os("EFFECTCRAFT_NO_RECOVERY").is_none() { session.begin_recovery() } else { None };
                if let Some(p) = files.iter().find(|f| is_project_file(f)) {
                    if let Err(e) = session.execute("file.open", json!({"path": p})) {
                        log::warn!("{p}: {e}");
                    }
                } else if demo {
                    let _ = session.execute("file.openDemoProject", json!({}));
                }
                let media: Vec<&String> = files.iter().filter(|f| !is_project_file(f)).collect();
                if !media.is_empty()
                    && let Err(e) = session.execute("file.import", json!({"paths": media}))
                {
                    log::warn!("import: {e}");
                }
                let show_home = home.unwrap_or(session.prefs.startup.show_home_on_launch && files.is_empty() && control_port.is_none());
                let _ = session.execute("window.restoreScriptPanels", json!({}));
                let mut app = EffectcraftApp::new(session);
                // air/OS updates the package (SoftwareUpdater); there is no upstream download.
                app.set_startup_update_check(false);
                app.ui.start_screen = show_home;
                if let Some(r) = recovery {
                    app.offer_recovery(r);
                }
                services::install(&mut app);
                if let Some(port) = control_port {
                    app = app.with_control(control_server::start(port, ctx.clone()));
                }
                let mut shell = shell::Shell::new(app, ctx, root, scale, renderer)?;
                shell.run();
                shell.finish();
                Ok(())
            });
            if let Err(e) = &result {
                log::error!("{e}");
                host::alert("EffectCraft", e);
            }
            host::quit();
            result
        })
        .map_err(|e| e.to_string())?;
    host::run();
    worker.join().map_err(|_| "the interface thread stopped unexpectedly".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_as_upstream_reads_them() {
        let parse = |a: &[&str]| parse_args(a.iter().map(|s| s.to_string()), None);
        assert_eq!(parse(&[]), Ok(Some(Args::default())));
        assert_eq!(parse(&["--version"]), Ok(None));
        let a = parse(&["--control", "9877", "--home", "a.ecproj"]).unwrap().unwrap();
        assert_eq!((a.control_port, a.home, a.files), (Some(9877), Some(true), vec!["a.ecproj".to_string()]));
        assert!(parse(&["--control", "0"]).is_err());
        assert!(parse(&["--nope"]).is_err());
        assert_eq!(parse(&["--", "--demo"]).unwrap().unwrap().files, vec!["--demo".to_string()]);
        assert_eq!(parse_args(Vec::new(), Some("9000".into())).unwrap().unwrap().control_port, Some(9000));
        assert!(is_project_file("x.ECPROJX") && !is_project_file("x.mov"));
    }

    #[test]
    fn at_most_four_workers_unless_asked() {
        assert_eq!(worker_threads(32, None), 4);
        assert_eq!(worker_threads(2, None), 2);
        assert_eq!(worker_threads(32, Some("8")), 8);
        assert_eq!(worker_threads(32, Some("0")), 4);
        assert_eq!(worker_threads(32, Some("x")), 4);
    }
}
