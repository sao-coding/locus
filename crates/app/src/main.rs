#![windows_subsystem = "windows"]
//! Locus: database client shell.
//!
//! `locus.exe` starts the product. `locus.exe <scene> [fixtures-dir]`
//! starts the same shell in bench mode for the local regression harness (see `dev/bench.rs`).

rust_i18n::i18n!("locales", fallback = "en");

mod activity;
mod connections;
#[cfg(feature = "dev")]
mod dev;
mod diagram;
mod history;
mod import;
mod palette;
mod panels;
mod prod;
mod query;
mod redis;
mod results;
mod routines;
mod saved;
mod settings;
mod shell;
mod structure;
mod update;
mod viewer;

use gpui_kit::component::TitleBar;
use gpui_kit::*;
use shell::Locus;
use std::time::Instant;

fn attach_console() {
    // The exe is a GUI app; in bench mode the harness reads stdout through a
    // pipe, which works without a console. When started from a terminal,
    // attach to it so the lines are visible there too.
    // SAFETY: `AttachConsole` takes no pointers; failing (no parent console) is harmless.
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(
            windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
        );
    }
}

fn main() {
    install_panic_hook();
    // Empty when built without sidecar/bin/sidecar.exe; unpacked only when Oracle is first used.
    locus_core::driver::set_embedded_sidecar(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/sidecar.bin"
    )));
    let t0 = Instant::now();
    let args: Vec<String> = std::env::args().collect();
    // Bench scenes exist only in `--features dev` builds.
    #[cfg(feature = "dev")]
    let scene = args
        .get(1)
        .filter(|a| dev::bench::SCENES.contains(&a.as_str()))
        .cloned();
    #[cfg(not(feature = "dev"))]
    let scene: Option<String> = None;
    #[cfg(feature = "dev")]
    let fixtures = args.get(2).cloned().unwrap_or_else(|| "../fixtures".into());
    #[cfg(not(feature = "dev"))]
    let _ = &args;
    if scene.is_some() {
        attach_console();
    }

    init_logging();
    let settings = locus_core::settings::Settings::load();
    let lang = match std::env::var("LOCUS_LANG") {
        Ok(l) => locus_core::settings::normalize(&l),
        Err(_) if scene.is_some() => locus_core::settings::system_language(),
        Err(_) => settings
            .language
            .as_deref()
            .map(locus_core::settings::normalize)
            .unwrap_or_else(locus_core::settings::system_language),
    };
    rust_i18n::set_locale(lang);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("db-driver")
        .enable_all()
        .build()
        .expect("tokio runtime");
    let handle = rt.handle().clone();

    #[cfg(feature = "dev")]
    let (initial_sql, extra, load_ms) = match &scene {
        Some(s) => dev::bench::initial_sql(s, &fixtures),
        None => (
            "SELECT * FROM student_real;".to_string(),
            String::new(),
            0.0,
        ),
    };
    #[cfg(not(feature = "dev"))]
    let initial_sql = "SELECT * FROM student_real;".to_string();

    gpui_kit::application()
        .with_assets(shell::icons::AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            settings::init(settings, cx);
            // After gpui_kit::init: at the same context depth the later binding wins.
            let keymap_errors = shell::keymap::init(cx);
            gpui_kit::component::set_locale(lang);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
            let mut opts = TitleBar::window_options();
            opts.window_bounds = Some(WindowBounds::Windowed(bounds));
            opts.window_min_size = Some(size(px(720.), px(480.)));
            if let Some(tb) = &mut opts.titlebar {
                tb.title = Some(rust_i18n::t!("app.title").to_string().into());
            }
            let persist = matches!(scene.as_deref(), Some("demo" | "history-type"))
                && std::env::var_os("LOCUS_DATA_DIR").is_some();
            let store = if scene.is_some() {
                locus_core::store::Store::Memory {
                    passwords: Vec::new(),
                    persist,
                }
            } else {
                locus_core::store::Store::Disk
            };
            #[cfg(feature = "dev")]
            let scene2 = scene.clone();
            let (_, wb) = gpui_kit::open_window(opts, cx, move |window, cx| {
                shell::apply_theme(window, cx);
                cx.new(|cx| Locus::new(store, handle, &initial_sql, window, cx))
            })
            .expect("open window");
            #[cfg(not(feature = "dev"))]
            let _ = (&wb, t0);
            // Data files that could not be read were kept as .bad-*.json (see locus_core::datafile).
            if !keymap_errors.is_empty() {
                shell::toast::show(
                    rust_i18n::t!("keymap.invalid", n = keymap_errors.len()).to_string(),
                    cx,
                );
            }
            for n in locus_core::datafile::take_notices() {
                shell::toast::show(
                    rust_i18n::t!("dataerr.datafile", file = n.file, backup = n.backup).to_string(),
                    cx,
                );
            }

            #[cfg(feature = "dev")]
            if let Some(scene) = scene2 {
                if let Some((c, pw)) = dev::bench::bench_connection() {
                    if scene == "demo" {
                        // A second entry to try switching connections mid-query.
                        let mut c2 = c.clone();
                        c2.id = "bench-2".into();
                        c2.name = format!("{} (2)", c.name);
                        // Test hook: LOCUS_DEMO_ENV=prod tags this second entry (named "prod-test").
                        if let Some(e) = std::env::var("LOCUS_DEMO_ENV")
                            .ok()
                            .and_then(|e| serde_json::from_str(&format!("\"{e}\"")).ok())
                        {
                            c2.env = e;
                            c2.name = "prod-test".into();
                        }
                        let pw2 = pw.clone();
                        wb.update(cx, |wb, cx| wb.add_ephemeral(c2, pw2, cx));
                    }
                    wb.update(cx, |wb, cx| wb.add_ephemeral(c, pw, cx));
                }
                let b = cx.new(|_| dev::bench::Bench::new(scene, wb.clone(), t0, extra, load_ms));
                wb.update(cx, |wb, _| {
                    wb.bench = Some(b.clone().into_any());
                    wb.first_frame = Some(Box::new(move |window, cx| {
                        dev::bench::Bench::boot(b, window, cx)
                    }));
                });
            }
        });
    rt.shutdown_timeout(std::time::Duration::from_secs(2));
}

/// Writes every panic with a backtrace to `%APPDATA%\Locus\crash.log`
/// (the release exe has no console), then runs the default hook.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let bt = std::backtrace::Backtrace::force_capture();
        let thread = std::thread::current().name().unwrap_or("?").to_string();
        let text = format!(
            "---- {:?} thread '{thread}' ----\n{info}\n{bt}\n",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        );
        if let Some(dir) = locus_core::settings::app_dir() {
            let _ = std::fs::create_dir_all(&dir);
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("crash.log"))
            {
                let _ = f.write_all(text.as_bytes());
            }
        }
        default(info);
    }));
}

/// Log file `<data dir>/locus.log` (info; `LOCUS_LOG=debug` for more). One
/// old file is kept: a log over 1 MB is moved to `locus.old.log` at start.
fn init_logging() {
    let Some(dir) = locus_core::settings::app_dir() else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("locus.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 1_000_000) {
        let _ = std::fs::rename(&path, dir.join("locus.old.log"));
    }
    let level = match std::env::var("LOCUS_LOG").as_deref() {
        Ok("debug") => log::LevelFilter::Debug,
        Ok("trace") => log::LevelFilter::Trace,
        _ => log::LevelFilter::Info,
    };
    if let Ok(f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let cfg = simplelog::ConfigBuilder::new()
            .set_time_format_rfc3339()
            .add_filter_allow_str("app")
            .add_filter_allow_str("locus_core")
            .add_filter_allow_str("db_")
            .build();
        let _ = simplelog::WriteLogger::init(level, cfg, f);
    }
}
