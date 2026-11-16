mod animation;
mod config;
mod font;
mod keys;
mod mux;
mod platform;
mod pty;
mod renderer;
mod scan;
mod status;
mod terminal;

fn main() -> std::process::ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    log_panics();
    platform::run(config::path())
}

/// Appends each panic with its backtrace to `~/Library/Logs/slutty/panic.log`:
/// launched from the Dock, stderr goes nowhere and a panic leaves no crash
/// report.
fn log_panics() {
    let Some(dir) = std::env::home_dir().map(|home| home.join("Library/Logs/slutty")) else { return };
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let entry = format!("--- unix time {secs}\n{info}\n{}\n", std::backtrace::Backtrace::force_capture());
        let written = std::fs::create_dir_all(&dir).and_then(|()| {
            let mut file = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("panic.log"))?;
            std::io::Write::write_all(&mut file, entry.as_bytes())
        });
        if let Err(err) = written {
            eprintln!("writing the panic log failed: {err}");
        }
        default(info);
    }));
}
