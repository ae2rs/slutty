//! Reading the config file. A missing file is the defaults; a broken one is
//! an error the caller shows while it keeps its last good config.

use std::io;
use std::path::{Path, PathBuf};

use super::settings::Config;

/// `$XDG_CONFIG_HOME/slutty/slutty.toml`, else `~/.config/slutty/slutty.toml`.
pub fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|home| home.join(".config")))?;
    Some(base.join("slutty").join("slutty.toml"))
}

/// The config in `path`, or why it cannot be used, in one line that starts
/// with the file's name.
pub fn load(path: &Path) -> Result<Config, String> {
    let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text).map_err(|err| format!("{name}: {err}")),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
        Err(err) => Err(format!("{name}: {err}")),
    }
}

/// Parses a config file. The error is one line, such as
/// "line 3: unknown field `famliy`, expected ...".
pub fn parse(text: &str) -> Result<Config, String> {
    toml::from_str(text).map_err(|err| match err.span() {
        Some(span) => format!("line {}: {}", text[..span.start].matches('\n').count() + 1, err.message().trim_end()),
        None => err.message().trim_end().to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::settings::{FontWeight, Length};

    #[test]
    fn an_empty_file_is_the_defaults() {
        assert_eq!(parse("").expect("valid"), Config::default());
    }

    #[test]
    fn the_design_example_parses() {
        let config = parse(
            r##"
            [font]
            family = "JetBrains Mono"
            size = 14
            line_height = 1.1
            bold_weight = "semibold"
            faint_weight = "light"

            [theme]
            name = "tokyo-night"
            colors = { background = "#000000", brights = ["#000001", "#000002", "#000003", "#000004", "#000005", "#000006", "#000007", "#000008"] }

            [window]
            padding = { left = "1cell", right = "2 cells", top = 4, bottom = "1.5pt" }

            [terminal]
            scrollback_lines = 10_000
            cursor = "block"
            option_as_alt = "both"

            [mouse]
            wheel_lines = 3

            [keys]
            leader = "ctrl+a"
            "##,
        )
        .expect("valid");
        assert_eq!(config.font.size, 14.0);
        assert_eq!(config.font.faint_weight, FontWeight::Light);
        assert_eq!(config.theme.background, [0, 0, 0]);
        assert_eq!(config.theme.ansi[15], [0, 0, 8]);
        assert_eq!(config.theme.ansi[0], Config::default().theme.ansi[0]);
        assert_eq!(config.window.padding.right, Length::Cells(2));
        assert_eq!(config.window.padding.top, Length::Points(4.0));
        assert_eq!(config.window.padding.bottom, Length::Points(1.5));
    }

    #[test]
    fn errors_name_the_key_and_line() {
        let err = parse("[font]\nsize = 12\nfamliy = \"x\"").expect_err("unknown key");
        assert!(err.contains("famliy") && err.contains("line 3"), "{err}");
        let err = parse("[terminal]\ncursor = \"triangle\"").expect_err("bad value");
        assert!(err.contains("triangle") && err.contains("line 2"), "{err}");
        let err = parse("[theme.colors]\nforeground = \"red\"").expect_err("bad color");
        assert!(err.contains("#rrggbb"), "{err}");
        let err = parse("[window]\npadding = { left = \"1furlong\" }").expect_err("bad length");
        assert!(err.contains("1furlong"), "{err}");
    }
}
