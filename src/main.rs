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
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use proto::Program;

/// A terminal and multiplexer. Its panes live in a server process, so they
/// outlive the app.
#[derive(Debug, Parser)]
#[command(name = "slutty", version)]
struct Args {
    /// The server's socket. Defaults to $SLUTTY_SOCKET, then to
    /// ~/Library/Application Support/slutty/mux.sock.
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    role: Option<Role>,
    /// What a new window runs instead of the login shell.
    #[arg(last = true, value_name = "COMMAND")]
    command: Vec<OsString>,
}

#[derive(Debug, Subcommand)]
enum Role {
    /// Runs the mux server, which the app starts by itself.
    Server {
        /// Goes to the background and logs to ~/Library/Logs/slutty.
        #[arg(long)]
        daemon: bool,
    },
}

fn main() -> ExitCode {
    // The Finder may pass `-psn_<n>` to an app it opens.
    let args = std::env::args_os().filter(|arg| !arg.as_encoded_bytes().starts_with(b"-psn_"));
    let args = Args::parse_from(args);
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    log_panics();
    // Inside a pane, the default socket is this very server: a second app
    // on it would take over from the one the pane is in.
    if args.role.is_none() && args.socket.is_none() && std::env::var_os("SLUTTY_PANE").is_some() {
        eprintln!("slutty: already inside slutty; pass --socket PATH for a separate server");
        return ExitCode::from(2);
    }
    let Some(socket) = proto::socket_path(args.socket) else {
        eprintln!("slutty: no home directory for the socket; pass --socket PATH");
        return ExitCode::FAILURE;
    };
    match args.role {
        Some(Role::Server { daemon }) => server::main(socket, daemon),
        None => platform::run(config::path(), &socket, first_program(args.command)),
    }
}

/// What a new window runs: the login shell, or `command` in this directory.
fn first_program(command: Vec<OsString>) -> Program {
    let mut command = command.into_iter();
    match command.next() {
        None => Program::default(),
        Some(program) => {
            Program { command: Some((program.into(), command.collect())), cwd: std::env::current_dir().ok() }
        }
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

    fn parse(args: &[&str]) -> Result<Args, clap::Error> {
        Args::try_parse_from(["slutty"].iter().chain(args))
    }

    #[test]
    fn no_argument_runs_the_login_shell() {
        let args = parse(&[]).expect("parses");
        assert!(args.role.is_none());
        assert_eq!(first_program(args.command), Program::default());
    }

    #[test]
    fn the_command_after_two_dashes_runs_as_is() {
        let args = parse(&["--socket", "/tmp/s", "--", "vtebench", "-b", "--"]).expect("parses");
        assert_eq!(args.socket.as_deref(), Some(std::path::Path::new("/tmp/s")));
        let program = first_program(args.command);
        let (path, args) = program.command.expect("a command");
        assert_eq!(path, std::path::Path::new("vtebench"));
        assert_eq!(args, ["-b", "--"]);
        assert_eq!(program.cwd, std::env::current_dir().ok());
    }

    #[test]
    fn the_server_role_takes_the_socket_after_it() {
        let args = parse(&["server", "--daemon", "--socket", "/tmp/s"]).expect("parses");
        assert!(matches!(args.role, Some(Role::Server { daemon: true })));
        assert!(args.socket.is_some());
    }

    #[test]
    fn anything_else_is_a_usage_error() {
        assert!(parse(&["vtebench"]).is_err());
        assert!(parse(&["--bogus"]).is_err());
    }
}
