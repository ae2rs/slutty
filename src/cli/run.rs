//! Runs one `slutty cli` command against the server: one request, or two
//! when a pane must be looked up, then the answer on stdout.

use std::io::{self, BufReader, Read};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::ExitCode;

use super::args::{Cli, CliCommand, Format, PaneArg, ProgramArgs};
use crate::client;
use crate::mux::{Axis, PaneId, WindowId};
use crate::proto::{self, ClientMessage, Command, PaneEntry, Program, Reply, Role, ServerMessage};

pub fn run(socket: &Path, cli: Cli) -> ExitCode {
    let result = Server::connect(socket).and_then(|mut server| server.execute(socket, cli.command));
    match result {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("slutty cli: {err}");
            ExitCode::FAILURE
        }
    }
}

struct Server {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    next_id: u32,
}

impl Server {
    fn connect(socket: &Path) -> Result<Self, String> {
        let stream = client::connect(socket, Role::Cli).map_err(|err| err.to_string())?;
        let reader = BufReader::new(stream.try_clone().map_err(|err| err.to_string())?);
        Ok(Self { stream, reader, next_id: 0 })
    }

    /// Runs `command`. Returns what to print.
    fn execute(&mut self, socket: &Path, command: CliCommand) -> Result<String, String> {
        match command {
            CliCommand::List { format } => {
                let entries = self.list()?;
                match format {
                    Format::Table => Ok(table(&entries)),
                    Format::Json => {
                        serde_json::to_string_pretty(&entries).map(|json| json + "\n").map_err(|e| e.to_string())
                    }
                }
            }
            CliCommand::Spawn { window_id, new_window, pane, program } => {
                let program = program_of(program)?;
                let command = if new_window {
                    Command::OpenWindow { program, size: None }
                } else {
                    let window = match window_id {
                        Some(window) => window,
                        None => {
                            let pane = self.pane(socket, pane)?;
                            self.window_of(pane)?
                        }
                    };
                    Command::NewTab { window, program }
                };
                self.spawned(command)
            }
            CliCommand::SplitPane { pane, right, down: _, percent, program } => {
                let pane = self.pane(socket, pane)?;
                let axis = if right { Axis::Row } else { Axis::Column };
                self.spawned(Command::Split { pane, axis, percent, program: program_of(program)? })
            }
            CliCommand::AdjustPaneSize { pane, direction, cells } => {
                let pane = self.pane(socket, pane)?;
                self.done(Command::ResizePane { pane, direction, cells })
            }
            CliCommand::ActivatePane { pane } => {
                let pane = self.pane(socket, pane)?;
                self.done(Command::ActivatePane { pane })
            }
            CliCommand::ActivatePaneDirection { pane, direction } => {
                let pane = self.pane(socket, pane)?;
                self.done(Command::ActivateDirection { pane, direction })
            }
            CliCommand::ZoomPane { pane } => {
                let pane = self.pane(socket, pane)?;
                self.done(Command::ToggleZoom { pane })
            }
            CliCommand::KillPane { pane } => {
                let pane = self.pane(socket, pane)?;
                self.done(Command::ClosePane { pane, confirm: false })
            }
            CliCommand::SendText { pane, no_paste, text } => {
                let pane = self.pane(socket, pane)?;
                let text = match text {
                    Some(text) => text,
                    None => {
                        let mut text = String::new();
                        io::stdin().read_to_string(&mut text).map_err(|err| format!("reading stdin: {err}"))?;
                        text
                    }
                };
                self.done(Command::SendText { pane, text, paste: !no_paste })
            }
            CliCommand::GetText { pane, start_line, end_line } => {
                let pane = self.pane(socket, pane)?;
                match self.request(Command::GetText { pane, start: start_line, end: end_line })? {
                    Reply::Text(text) => Ok(text),
                    other => Err(unexpected(&other)),
                }
            }
            CliCommand::KillServer => self.done(Command::KillServer),
        }
    }

    fn request(&mut self, command: Command) -> Result<Reply, String> {
        self.next_id += 1;
        let id = self.next_id;
        proto::send(&mut &self.stream, &ClientMessage::Request { id, command }).map_err(|err| err.to_string())?;
        loop {
            match proto::receive(&mut self.reader).map_err(|err| err.to_string())? {
                Some(ServerMessage::Response { id: answered, result }) if answered == id => return result,
                Some(_) => {}
                None => return Err("the server hung up".to_owned()),
            }
        }
    }

    fn done(&mut self, command: Command) -> Result<String, String> {
        match self.request(command)? {
            Reply::Done => Ok(String::new()),
            other => Err(unexpected(&other)),
        }
    }

    /// Sends a command that makes a pane, and returns its id to print.
    fn spawned(&mut self, command: Command) -> Result<String, String> {
        match self.request(command)? {
            Reply::Spawned { pane, .. } => Ok(format!("{pane}\n")),
            other => Err(unexpected(&other)),
        }
    }

    fn list(&mut self) -> Result<Vec<PaneEntry>, String> {
        match self.request(Command::List)? {
            Reply::List(entries) => Ok(entries),
            other => Err(unexpected(&other)),
        }
    }

    /// `--pane-id`, else `$SLUTTY_PANE` when it names a pane of this server,
    /// else the focused pane.
    fn pane(&mut self, socket: &Path, arg: PaneArg) -> Result<PaneId, String> {
        if let Some(pane) = arg.pane_id {
            return Ok(pane);
        }
        let ours = std::env::var_os("SLUTTY_SOCKET").is_some_and(|ours| Path::new(&ours) == socket);
        if let Some(pane) = std::env::var("SLUTTY_PANE").ok().filter(|_| ours) {
            return pane.parse().map_err(|_| format!("$SLUTTY_PANE is not a pane id: {pane}"));
        }
        let entries = self.list()?;
        entries.iter().find(|e| e.focused).map(|e| e.pane_id).ok_or_else(|| "no pane to act on".to_owned())
    }

    fn window_of(&mut self, pane: PaneId) -> Result<WindowId, String> {
        let entries = self.list()?;
        entries.iter().find(|e| e.pane_id == pane).map(|e| e.window_id).ok_or_else(|| format!("no pane {pane}"))
    }
}

/// A relative `--cwd` is read from here, not from the server's directory.
fn program_of(args: ProgramArgs) -> Result<Program, String> {
    let cwd = match args.cwd {
        Some(cwd) => Some(std::path::absolute(&cwd).map_err(|err| format!("--cwd {}: {err}", cwd.display()))?),
        None => None,
    };
    let mut command = args.command.into_iter();
    Ok(Program { command: command.next().map(|program| (program.into(), command.collect())), cwd })
}

fn unexpected(reply: &Reply) -> String {
    format!("unexpected reply from the server: {reply:?}")
}

/// One line per pane under a header, columns padded to their widest cell.
fn table(entries: &[PaneEntry]) -> String {
    let header = ["WINDOW", "TAB", "PANE", "SIZE", "TITLE", "CWD"].map(str::to_owned);
    let rows = entries.iter().map(|e| {
        let cwd = e.cwd.as_ref().map(|cwd| cwd.display().to_string()).unwrap_or_default();
        let pane = format!("{}{}", e.pane_id, if e.focused { "*" } else { "" });
        [e.window_id.to_string(), e.tab_index.to_string(), pane, format!("{}x{}", e.cols, e.rows), e.title.clone(), cwd]
    });
    let rows: Vec<[String; 6]> = std::iter::once(header).chain(rows).collect();
    let widths: Vec<usize> = (0..6).map(|i| rows.iter().map(|row| row[i].chars().count()).max().unwrap_or(0)).collect();
    let mut out = String::new();
    for row in &rows {
        let cells: Vec<String> = row.iter().zip(&widths).map(|(cell, &width)| format!("{cell:width$}")).collect();
        out.push_str(cells.join("  ").trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use std::ffi::OsString;

    use super::*;
    use crate::server::{self, Listener};

    fn cli(socket: &Path, command: CliCommand) -> Result<String, String> {
        Server::connect(socket)?.execute(socket, command)
    }

    fn pane(pane_id: Option<PaneId>) -> PaneArg {
        PaneArg { pane_id }
    }

    fn program(command: &[&str]) -> ProgramArgs {
        ProgramArgs { cwd: None, command: command.iter().map(OsString::from).collect() }
    }

    fn list(socket: &Path) -> Vec<serde_json::Value> {
        let json = cli(socket, CliCommand::List { format: Format::Json }).expect("listed");
        serde_json::from_str(&json).expect("json")
    }

    #[test]
    fn the_cli_drives_panes_on_a_headless_server() {
        let dir = std::env::temp_dir().join(format!("slutty-cli-{}", std::process::id()));
        let socket = dir.join("mux.sock");
        let listener = Listener::bind(&socket).expect("binds").expect("free");
        let (ready, started) = mpsc::channel();
        let server = thread::spawn(move || server::run(listener, None, false, move || ready.send(()).expect("told")));
        started.recv().expect("ready");

        let spawn =
            CliCommand::Spawn { window_id: None, new_window: true, pane: pane(None), program: program(&["sh"]) };
        let first: PaneId = cli(&socket, spawn).expect("spawned").trim().parse().expect("a pane id");
        // No --pane-id and no $SLUTTY_PANE for this server: the focused pane.
        let split = CliCommand::SplitPane {
            pane: pane(None),
            right: true,
            down: false,
            percent: Some(25),
            program: program(&[]),
        };
        let second: PaneId = cli(&socket, split).expect("split").trim().parse().expect("a pane id");

        let panes = list(&socket);
        assert_eq!(panes.len(), 2);
        let keys: Vec<&str> = panes[0].as_object().expect("an object").keys().map(String::as_str).collect();
        let expected = [
            "active",
            "cols",
            "cwd",
            "floating",
            "focused",
            "pane_id",
            "process",
            "rho",
            "rows",
            "tab_index",
            "title",
            "window_id",
            "x",
            "y",
            "zoomed",
        ];
        assert_eq!(keys, expected, "the JSON fields rho reads");
        assert_eq!(panes[1]["pane_id"], second.to_string().parse::<u64>().expect("a number"));
        assert_eq!(panes[1]["focused"], true, "a split takes the focus");
        assert_eq!(panes[0]["cols"].as_u64().expect("cols") + panes[1]["cols"].as_u64().expect("cols") + 1, 80);

        let send = CliCommand::SendText {
            pane: pane(Some(first)),
            no_paste: true,
            text: Some("echo marker-$((6*7))\r".into()),
        };
        cli(&socket, send).expect("sent");
        let start = Instant::now();
        loop {
            let get = CliCommand::GetText { pane: pane(Some(first)), start_line: 0, end_line: None };
            if cli(&socket, get).expect("text").contains("marker-42") {
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(10), "the shell never echoed");
            thread::sleep(Duration::from_millis(20));
        }

        cli(&socket, CliCommand::KillPane { pane: pane(Some(second)) }).expect("killed");
        assert_eq!(list(&socket).len(), 1);
        assert!(cli(&socket, CliCommand::ZoomPane { pane: pane(Some(second)) }).is_err(), "the pane is gone");
        cli(&socket, CliCommand::KillServer).expect("stopped");
        server.join().expect("no panic").expect("served");
        assert_eq!(cli(&socket, CliCommand::KillServer), Err("no slutty server running".to_owned()));
        std::fs::remove_dir_all(dir).expect("cleaned up");
    }

    #[test]
    fn the_table_pads_columns_and_marks_the_focused_pane() {
        let entry = |pane: u64, focused| PaneEntry {
            window_id: "1".parse().expect("id"),
            tab_index: 0,
            pane_id: pane.to_string().parse().expect("id"),
            x: 0,
            y: 0,
            cols: 80,
            rows: 24,
            title: "zsh".into(),
            cwd: Some("/tmp".into()),
            process: Some("zsh".into()),
            rho: None,
            active: focused,
            focused,
            zoomed: false,
            floating: None,
        };
        let table = table(&[entry(2, false), entry(13, true)]);
        assert_eq!(
            table,
            "WINDOW  TAB  PANE  SIZE   TITLE  CWD\n1       0    2     80x24  zsh    /tmp\n1       0    13*   80x24  zsh    /tmp\n"
        );
    }
}
