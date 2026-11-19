//! What `slutty cli` takes, named after `wezterm cli` where it has a
//! counterpart.

use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

use crate::mux::{Anchor, Direction, Extent, PaneId, PlaceChange, WindowId};

#[derive(Debug, Args)]
pub struct Cli {
    #[command(subcommand)]
    pub command: CliCommand,
}

#[derive(Debug, Subcommand)]
pub enum CliCommand {
    /// Lists every pane.
    List {
        #[arg(long, value_enum, default_value_t = Format::Table)]
        format: Format,
    },
    /// Opens a tab, or a window, running PROG or the login shell. Prints
    /// the new pane's id.
    Spawn {
        /// The window to open the tab in; else the pane's window.
        #[arg(long, conflicts_with = "new_window")]
        window_id: Option<WindowId>,
        /// Opens a window instead of a tab.
        #[arg(long)]
        new_window: bool,
        #[command(flatten)]
        pane: PaneArg,
        #[command(flatten)]
        program: ProgramArgs,
    },
    /// Splits a pane, below unless --right. Prints the new pane's id.
    SplitPane {
        #[command(flatten)]
        pane: PaneArg,
        #[arg(long, conflicts_with = "down")]
        right: bool,
        #[arg(long)]
        down: bool,
        /// The new pane's share of the space; else half.
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=99))]
        percent: Option<u8>,
        #[command(flatten)]
        program: ProgramArgs,
    },
    /// Opens a floating pane over the tab of a pane, running PROG or the
    /// login shell: centred, 80% by 80%, unless told otherwise. Prints the
    /// new pane's id.
    FloatPane {
        #[command(flatten)]
        pane: PaneArg,
        #[command(flatten)]
        place: PlaceArgs,
        /// Starts it hidden, without the keys; show-pane shows it.
        #[arg(long)]
        hidden: bool,
        #[command(flatten)]
        program: ProgramArgs,
    },
    /// Moves or resizes a floating pane. What is not given stays.
    PlacePane {
        #[command(flatten)]
        pane: PaneArg,
        #[command(flatten)]
        place: PlaceArgs,
    },
    /// Hides a floating pane. Its program runs on.
    HidePane {
        #[command(flatten)]
        pane: PaneArg,
    },
    /// Shows a floating pane on top and gives it the keys.
    ShowPane {
        #[command(flatten)]
        pane: PaneArg,
    },
    /// Moves the divider nearest a pane, or grows a floating pane: right
    /// and down grow it.
    AdjustPaneSize {
        #[command(flatten)]
        pane: PaneArg,
        #[arg(long, value_enum)]
        direction: Direction,
        #[arg(long, default_value_t = 1)]
        cells: u16,
    },
    /// Shows a pane and gives it the keys.
    ActivatePane {
        #[command(flatten)]
        pane: PaneArg,
    },
    /// Activates the pane next to a pane.
    ActivatePaneDirection {
        #[command(flatten)]
        pane: PaneArg,
        #[arg(value_enum)]
        direction: Direction,
    },
    /// Zooms a pane, or ends the zoom of its tab.
    ZoomPane {
        #[command(flatten)]
        pane: PaneArg,
    },
    /// Closes a pane, whatever runs in it.
    KillPane {
        #[command(flatten)]
        pane: PaneArg,
    },
    /// Sends TEXT, or stdin, to a pane, as a paste unless --no-paste.
    SendText {
        #[command(flatten)]
        pane: PaneArg,
        /// Sends the text as typed: no bracketed paste.
        #[arg(long)]
        no_paste: bool,
        text: Option<String>,
    },
    /// Prints a pane's lines as plain text: the screen, unless the lines
    /// say otherwise.
    GetText {
        #[command(flatten)]
        pane: PaneArg,
        /// 0 is the top row of the screen; negative lines are scrollback.
        #[arg(long, allow_negative_numbers = true, default_value_t = 0)]
        start_line: i32,
        /// Else the bottom row of the screen.
        #[arg(long, allow_negative_numbers = true)]
        end_line: Option<i32>,
    },
    /// Closes every pane and stops the server.
    KillServer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Table,
    Json,
}

#[derive(Debug, Args)]
pub struct PaneArg {
    /// The pane; else $SLUTTY_PANE when on its server, else the focused pane.
    #[arg(long)]
    pub pane_id: Option<PaneId>,
}

#[derive(Clone, Copy, Debug, Default, Args)]
pub struct PlaceArgs {
    /// The point of the tab it sticks to.
    #[arg(long, value_enum)]
    pub anchor: Option<Anchor>,
    /// 40% of the tab, or 40 cells.
    #[arg(long)]
    pub width: Option<Extent>,
    #[arg(long)]
    pub height: Option<Extent>,
    /// Cells right of the anchored spot; negative goes left.
    #[arg(long, allow_negative_numbers = true)]
    pub x_offset: Option<i16>,
    /// Cells below the anchored spot; negative goes up.
    #[arg(long, allow_negative_numbers = true)]
    pub y_offset: Option<i16>,
}

impl PlaceArgs {
    pub fn change(&self) -> PlaceChange {
        let Self { anchor, width, height, x_offset, y_offset } = *self;
        PlaceChange { anchor, width, height, x_offset, y_offset }
    }
}

#[derive(Debug, Args)]
pub struct ProgramArgs {
    /// Where the program starts; else the pane's directory.
    #[arg(long)]
    pub cwd: Option<PathBuf>,
    /// What runs instead of the login shell.
    #[arg(last = true, value_name = "PROG")]
    pub command: Vec<OsString>,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Debug, Parser)]
    struct Line {
        #[command(subcommand)]
        command: CliCommand,
    }

    fn parse(args: &[&str]) -> Result<CliCommand, clap::Error> {
        Line::try_parse_from(["cli"].iter().chain(args)).map(|line| line.command)
    }

    #[test]
    fn a_program_follows_two_dashes() {
        let Ok(CliCommand::SplitPane { right, percent, program, .. }) =
            parse(&["split-pane", "--right", "--percent", "30", "--cwd", "src", "--", "vim", "a b"])
        else {
            panic!("a split");
        };
        assert!(right);
        assert_eq!(percent, Some(30));
        assert_eq!(program.cwd.as_deref(), Some(std::path::Path::new("src")));
        assert_eq!(program.command, ["vim", "a b"]);
    }

    #[test]
    fn lines_can_be_negative_and_panes_are_numbers() {
        let Ok(CliCommand::GetText { pane, start_line, end_line }) =
            parse(&["get-text", "--pane-id", "7", "--start-line", "-20", "--end-line", "-1"])
        else {
            panic!("get-text");
        };
        assert_eq!(pane.pane_id, Some("7".parse().expect("id")));
        assert_eq!((start_line, end_line), (-20, Some(-1)));
        assert!(parse(&["get-text", "--pane-id", "seven"]).is_err());
    }

    #[test]
    fn floats_take_an_anchor_sizes_and_signed_offsets() {
        let Ok(CliCommand::FloatPane { place, hidden, program, .. }) = parse(&[
            "float-pane",
            "--anchor",
            "top-right",
            "--width",
            "40%",
            "--height",
            "12",
            "--x-offset",
            "-2",
            "--hidden",
            "--",
            "htop",
        ]) else {
            panic!("float-pane");
        };
        let change = place.change();
        assert_eq!(change.anchor, Some(Anchor::TopRight));
        assert_eq!((change.width, change.height), (Some(Extent::Percent(40)), Some(Extent::Cells(12))));
        assert_eq!((change.x_offset, change.y_offset), (Some(-2), None));
        assert!(hidden);
        assert_eq!(program.command, ["htop"]);
        assert!(parse(&["place-pane", "--width", "0"]).is_err());
        assert!(parse(&["place-pane", "--anchor", "middle"]).is_err());
    }

    #[test]
    fn bad_values_are_usage_errors() {
        assert!(parse(&["split-pane", "--right", "--down"]).is_err());
        assert!(parse(&["split-pane", "--percent", "100"]).is_err());
        assert!(parse(&["adjust-pane-size", "--direction", "sideways"]).is_err());
        assert!(parse(&["spawn", "--window-id", "1", "--new-window"]).is_err());
        assert!(parse(&["list", "--format", "yaml"]).is_err());
    }
}
