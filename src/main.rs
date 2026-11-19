mod animation;
mod client;
mod config;
mod font;
mod keys;
mod mux;
mod platform;
mod proto;
mod pty;
mod renderer;
mod scan;
mod server;
mod status;
mod terminal;

use std::ffi::OsString;

use proto::Program;

const USAGE: &str = "usage: slutty [-- <command> [<arg>...]]";

fn main() -> std::process::ExitCode {
    let Some(first) = first_program(std::env::args_os().skip(1)) else {
        eprintln!("{USAGE}");
        return std::process::ExitCode::from(2);
    };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    log_panics();
    platform::run(config::path(), first)
}

/// What the first window runs: the login shell, or the command after `--`
/// in this directory. `None` for any other arguments.
fn first_program(args: impl IntoIterator<Item = OsString>) -> Option<Program> {
    // The Finder may pass `-psn_<n>` to an app it opens.
    let mut args = args.into_iter().filter(|arg| !arg.as_encoded_bytes().starts_with(b"-psn_"));
    match args.next() {
        None => Some(Program::default()),
        Some(arg) if arg == "--" => {
            let program = args.next()?;
            Some(Program { command: Some((program.into(), args.collect())), cwd: std::env::current_dir().ok() })
        }
        Some(_) => None,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Option<Program> {
        first_program(args.iter().map(OsString::from))
    }

    #[test]
    fn no_argument_runs_the_login_shell() {
        assert_eq!(parse(&[]), Some(Program::default()));
        assert_eq!(parse(&["-psn_0_12345"]), Some(Program::default()));
    }

    #[test]
    fn the_command_after_two_dashes_runs_as_is() {
        let program = parse(&["--", "vtebench", "-b", "--"]).expect("a command");
        let (path, args) = program.command.expect("a command");
        assert_eq!(path, std::path::Path::new("vtebench"));
        assert_eq!(args, ["-b", "--"]);
        assert_eq!(program.cwd, std::env::current_dir().ok());
    }

    #[test]
    fn anything_else_is_a_usage_error() {
        assert!(parse(&["--"]).is_none());
        assert!(parse(&["vtebench"]).is_none());
        assert!(parse(&["--help"]).is_none());
    }
}
