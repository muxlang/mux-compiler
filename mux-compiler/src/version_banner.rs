use anstream::{println, stdout};
use anstyle::AnsiColor;
use std::fmt::Write as _;
use std::io::Write;
use std::thread::sleep;
use std::time::Duration;

const LOGO_ROWS: usize = 6;
const ANIMATION_ROWS: usize = LOGO_ROWS + 3;
const TOTAL_COLUMNS: usize = 30;
const MESSAGE: &str = "The Programming Language For Everyone";

pub(crate) fn print(linker_version: Option<&str>, llvm_version: &str) {
    let palette = Palette::new();
    let green = AnsiColor::Green.on_default().bold();
    let combined = logo_rows();
    let version_lines = version_lines(green, linker_version, llvm_version);
    let num_versions = version_lines.len();

    // Allocate blank space for the animated section
    for _ in 0..ANIMATION_ROWS {
        println!();
    }
    // Version info below the animated area, visible from the start
    for version in &version_lines {
        println!("{version}");
    }

    let mut out = stdout();
    out.flush().ok();

    // Neon gradient column wipe (1 col per frame)
    for columns in 1..=TOTAL_COLUMNS {
        let offset = if columns == 1 {
            ANIMATION_ROWS + num_versions
        } else {
            ANIMATION_ROWS
        };
        let buffer = sweep_frame(&combined, columns, offset, &palette);
        write!(out, "{buffer}").ok();
        out.flush().ok();
        sleep(Duration::from_millis(10));
    }

    // Settle frame: render the full logo in settled blue
    sleep(Duration::from_millis(10));
    let buffer = settled_frame(&combined, &palette.settled);
    write!(out, "{buffer}").ok();
    out.flush().ok();

    // Pause to show the completed logo before moving on
    sleep(Duration::from_millis(200));

    // Move cursor past the version lines so the shell prompt doesn't overlap
    write!(out, "\x1b[{num_versions}B").ok();
    out.flush().ok();
}

struct Palette {
    settled: anstyle::Style,
    warm: anstyle::Style,
    glow: anstyle::Style,
    hot: anstyle::Style,
}

impl Palette {
    fn new() -> Self {
        let rgb = |red, green, blue| {
            anstyle::Style::new().fg_color(Some(anstyle::RgbColor(red, green, blue).into()))
        };
        Self {
            settled: rgb(0x60, 0xa5, 0xfa),
            warm: rgb(0x93, 0xc5, 0xfd),
            glow: rgb(0xbf, 0xdb, 0xfe),
            hot: rgb(0xff, 0xff, 0xff),
        }
    }

    /// Style for a column that trails the sweep head by `distance` columns.
    fn for_distance(&self, distance: usize) -> &anstyle::Style {
        match distance {
            0..=1 => &self.hot,
            2..=4 => &self.warm,
            5..=8 => &self.glow,
            _ => &self.settled,
        }
    }
}

/// Right-pad or truncate `content` to exactly `width` characters.
fn pad(content: &str, width: usize) -> String {
    let mut chars: Vec<char> = content.chars().collect();
    chars.truncate(width);
    let mut padded: String = chars.into_iter().collect();
    while padded.chars().count() < width {
        padded.push(' ');
    }
    padded
}

/// Combined M-U-X logo bitmap: 6 rows, 30 chars wide (11+1+9+1+8).
fn logo_rows() -> Vec<Vec<char>> {
    let m: [&str; LOGO_ROWS] = [
        "███╗   ███╗",
        "████╗ ████║",
        "██╔████╔██║",
        "██║╚██╔╝██║",
        "██║ ╚═╝ ██║",
        "╚═╝     ╚═╝",
    ];
    let u: [&str; LOGO_ROWS] = [
        "██╗   ██╗",
        "██║   ██║",
        "██║   ██║",
        "██║   ██║",
        "╚██████╔╝",
        " ╚═════╝",
    ];
    let x: [&str; LOGO_ROWS] = [
        "██╗  ██╗",
        "╚██╗██╔╝",
        " ╚███╔╝",
        " ██╔██╗",
        "██╔╝ ██╗",
        "╚═╝  ╚═╝",
    ];
    (0..LOGO_ROWS)
        .map(|row| {
            let mut line = String::new();
            line.push_str(&pad(m[row], 11));
            line.push(' ');
            line.push_str(&pad(u[row], 9));
            line.push(' ');
            line.push_str(&pad(x[row], 8));
            line.chars().collect()
        })
        .collect()
}

fn version_lines(
    green: anstyle::Style,
    linker_version: Option<&str>,
    llvm_version: &str,
) -> Vec<String> {
    let mut lines = vec![
        format!("{green}compiler{green:#} v{}", env!("CARGO_PKG_VERSION")),
        format!("{green}runtime{green:#} v{}", env!("MUX_RUNTIME_VERSION")),
    ];
    if let Some(version) = linker_version {
        lines.push(format!("{green}clang{green:#} v{version}"));
    }
    lines.push(format!("{green}llvm{green:#} v{llvm_version}"));
    lines
}

/// One animation frame: the logo revealed up to `columns` columns, with a
/// neon gradient trailing the sweep head. Starts by moving the cursor up
/// `offset` rows so the frame redraws in place.
fn sweep_frame(combined: &[Vec<char>], columns: usize, offset: usize, palette: &Palette) -> String {
    let mut buffer = format!("\x1b[{offset}A");
    // Blank line before logo
    buffer.push('\n');
    for char_row in combined {
        for (column, character) in char_row.iter().enumerate().take(columns) {
            let style = palette.for_distance(columns - 1 - column);
            let _ = write!(&mut buffer, "{style}{character}{style:#}");
        }
        buffer.push('\n');
    }
    buffer.push('\n');
    let settled = &palette.settled;
    let _ = writeln!(&mut buffer, "{settled}{MESSAGE}{settled:#}");
    buffer
}

fn settled_frame(combined: &[Vec<char>], settled: &anstyle::Style) -> String {
    let mut buffer = format!("\x1b[{ANIMATION_ROWS}A");
    buffer.push('\n');
    for char_row in combined {
        let line: String = char_row.iter().collect();
        let _ = writeln!(&mut buffer, "{settled}{line}{settled:#}");
    }
    buffer.push('\n');
    let _ = writeln!(&mut buffer, "{settled}{MESSAGE}{settled:#}");
    buffer
}

#[cfg(test)]
mod tests {
    use super::{LOGO_ROWS, Palette, logo_rows, settled_frame, sweep_frame, version_lines};
    use anstyle::AnsiColor;

    #[test]
    fn logo_has_the_expected_dimensions() {
        let rows = logo_rows();
        assert_eq!(rows.len(), LOGO_ROWS);
        assert!(rows.iter().all(|row| row.len() == 30));
    }

    #[test]
    fn version_lines_include_resolved_tool_versions() {
        let green = AnsiColor::Green.on_default().bold();
        let lines = version_lines(green, Some("17.0.6"), "22.1.0");
        assert!(
            lines
                .iter()
                .any(|line| line.contains("clang") && line.contains("17.0.6"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("llvm") && line.contains("22.1.0"))
        );
    }

    #[test]
    fn frames_preserve_cursor_offsets_and_message() {
        let rows = logo_rows();
        let palette = Palette::new();
        let sweep = sweep_frame(&rows, 1, 12, &palette);
        assert!(sweep.starts_with("\x1b[12A"));
        assert!(sweep.contains(super::MESSAGE));

        let settled = settled_frame(&rows, &palette.settled);
        assert!(settled.starts_with("\x1b[9A"));
        assert!(settled.contains(super::MESSAGE));
    }
}
