use std::io;
use std::sync::Once;

use chrono::Local;
use fern::Dispatch;
use log::LevelFilter;

fn logging_level() -> LevelFilter {
    // 1. Check for debug files near executable
    if let Ok(exe_path) = std::env::current_exe()
        && let Some(dir) = exe_path.parent()
    {
        if dir.join("trace").exists() {
            return LevelFilter::Trace;
        }
        if dir.join("debug").exists() {
            return LevelFilter::Debug;
        }
    }
    match std::env::var("BROKER_DEBUG").as_deref() {
        Ok("trace") => LevelFilter::Trace,
        Ok("debug") => LevelFilter::Debug,
        Ok("info") => LevelFilter::Info,
        Ok("warn") => LevelFilter::Warn,
        Ok("error") => LevelFilter::Error,
        _ => LevelFilter::Info, // default if unset or unknown
    }
}

pub fn setup_logger() {
    static INIT: Once = Once::new();

    let level_filter = logging_level();

    INIT.call_once(move || {
        // Attempt to install the global logger once. If installation fails
        // (for example because another logger is already installed), emit a
        // diagnostic to stderr but do not panic or attempt re-installation.
        if let Err(e) = Dispatch::new()
            .format(move |out, message, record| {
                let file = record.file().unwrap_or("unknown_file");
                let line = record.line().map_or(0, |l| l);

                out.finish(format_args!(
                    "[{}][{}]: {} <{}:{}>",
                    Local::now().format("%b-%d-%Y %H:%M:%S.%f"),
                    record.level(),
                    message,
                    file,
                    line,
                ));
            })
            .level(level_filter)
            .chain(io::stdout())
            .apply()
        {
            eprintln!("Logger initialization failed: {e}");
        }
    });

    log::debug!("Enabled log {level_filter}.");
}
