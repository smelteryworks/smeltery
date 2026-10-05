//! Presentation of the `smeltery` command line: colours, the banner, question sections, badges, progress lines,
//! the spinner and the summary box.
//!
//! Styling is on only when stdout is a terminal that understands ANSI escapes and neither `NO_COLOR` nor
//! `--no-color` is given. Otherwise every caller prints the plain text it printed before styling existed, so
//! scripts, tests and CI see the same output. `SMELTERY_FORCE_STYLE=1` turns styling on for non-terminal output,
//! which is how the styled screens are previewed (rendered into a file) without a terminal window.

mod font;

use std::fmt::Write as _;
use std::io::{IsTerminal, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::style::{Attribute, Color, Stylize};

/// Widest line the banner may use.
pub(crate) const BANNER_WIDTH: usize = 80;

/// The tagline under the banner.
const TAGLINE: &str = "Batteries-included full-stack Rust";

/// Env var forcing styled output even when stdout is not a terminal (previews).
const FORCE_ENV: &str = "SMELTERY_FORCE_STYLE";

/// How the CLI presents its output.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Ui {
    styled: bool,
    /// 24-bit colour; otherwise the 256-colour palette (macOS Terminal has no true colour).
    truecolor: bool,
    /// Stdout is a terminal, so the spinner may redraw a line.
    tty: bool,
    /// Styling was forced by [`FORCE_ENV`] (preview mode).
    forced: bool,
}

/// A coloured label in front of a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Badge {
    Info,
    Warn,
    Done,
    Error,
}

impl Badge {
    fn label(self) -> &'static str {
        match self {
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Done => "DONE",
            Self::Error => "ERROR",
        }
    }

    fn colors(self) -> (Color, Color) {
        match self {
            Self::Info => (
                Color::Rgb {
                    r: 37,
                    g: 99,
                    b: 235,
                },
                Color::White,
            ),
            Self::Warn => (
                Color::Rgb {
                    r: 234,
                    g: 179,
                    b: 8,
                },
                Color::Black,
            ),
            Self::Done => (
                Color::Rgb {
                    r: 22,
                    g: 163,
                    b: 74,
                },
                Color::White,
            ),
            Self::Error => (
                Color::Rgb {
                    r: 220,
                    g: 38,
                    b: 38,
                },
                Color::White,
            ),
        }
    }

    fn ansi(self) -> (Color, Color) {
        match self {
            Self::Info => (Color::AnsiValue(25), Color::White),
            Self::Warn => (Color::AnsiValue(178), Color::Black),
            Self::Done => (Color::AnsiValue(28), Color::White),
            Self::Error => (Color::AnsiValue(160), Color::White),
        }
    }
}

/// The two ends of the banner gradient: ember orange to gold.
const EMBER: (u8, u8, u8) = (249, 115, 22);
const GOLD: (u8, u8, u8) = (250, 204, 21);
/// The same gradient in the 256-colour palette.
const EMBER_256: [u8; 5] = [202, 208, 214, 220, 226];

impl Ui {
    /// Decide from the terminal, `NO_COLOR`, `--no-color` and [`FORCE_ENV`].
    pub(crate) fn detect(no_color_flag: bool) -> Self {
        let no_color = no_color_flag || env_set("NO_COLOR");
        let forced = env_set(FORCE_ENV) && std::env::var(FORCE_ENV).ok().as_deref() != Some("0");
        let tty = std::io::stdout().is_terminal();
        let dumb = std::env::var("TERM").is_ok_and(|t| t == "dumb");
        let styled = !no_color && (forced || (tty && !dumb && ansi_ok()));
        let truecolor = forced
            || std::env::var("COLORTERM").is_ok_and(|v| v == "truecolor" || v == "24bit")
            || std::env::var("WT_SESSION").is_ok();
        Self {
            styled,
            truecolor,
            tty,
            forced: forced && styled,
        }
    }

    /// No styling at all.
    #[cfg(test)]
    pub(crate) fn plain() -> Self {
        Self {
            styled: false,
            truecolor: false,
            tty: false,
            forced: false,
        }
    }

    /// Styled output with true colour and no terminal (tests and previews).
    ///
    /// crossterm writes no colour codes at all when `NO_COLOR` is set in the environment
    /// (crossterm 0.29.0 `src/style/types/colored.rs:100`), so the test process forces them on, keeping the
    /// styled tests independent of the developer's shell. Test builds only: a real run still honours `NO_COLOR`
    /// (in [`Ui::detect`] and in crossterm).
    #[cfg(test)]
    pub(crate) fn styled_for_test() -> Self {
        crossterm::style::force_color_output(true);
        Self {
            styled: true,
            truecolor: true,
            tty: false,
            forced: true,
        }
    }

    /// Whether styling is on.
    pub(crate) fn styled(self) -> bool {
        self.styled
    }

    /// Whether styling was forced for a preview (questions are shown as answered sections).
    pub(crate) fn forced(self) -> bool {
        self.forced
    }

    fn rgb(self, (r, g, b): (u8, u8, u8), fallback: u8) -> Color {
        if self.truecolor {
            Color::Rgb { r, g, b }
        } else {
            Color::AnsiValue(fallback)
        }
    }

    fn ember(self) -> Color {
        self.rgb(EMBER, 208)
    }

    fn gold(self) -> Color {
        self.rgb(GOLD, 220)
    }

    fn green(self) -> Color {
        self.rgb((74, 222, 128), 78)
    }

    fn red(self) -> Color {
        self.rgb((248, 113, 113), 203)
    }

    fn dim(self, text: &str) -> String {
        if self.styled {
            text.with(Color::DarkGrey).to_string()
        } else {
            text.to_owned()
        }
    }

    /// The colour at position `i` of `n` along the banner gradient.
    fn gradient(self, i: usize, n: usize) -> Color {
        let n = n.max(2) - 1;
        let i = i.min(n);
        if self.truecolor {
            let mix = |a: u8, b: u8| -> u8 {
                let (a, b) = (u32::from(a), u32::from(b));
                let v = (a * (n - i.min(n)) as u32 + b * i as u32) / n.max(1) as u32;
                u8::try_from(v).unwrap_or(u8::MAX)
            };
            Color::Rgb {
                r: mix(EMBER.0, GOLD.0),
                g: mix(EMBER.1, GOLD.1),
                b: mix(EMBER.2, GOLD.2),
            }
        } else {
            let idx = i * (EMBER_256.len() - 1) / n.max(1);
            Color::AnsiValue(EMBER_256.get(idx).copied().unwrap_or(208))
        }
    }

    /// The banner: SMELTERY in block letters with a left-to-right gradient, a rule, and a dim subtitle with the
    /// tagline and `version`. Every line is at most [`BANNER_WIDTH`] columns.
    pub(crate) fn banner(self, version: &str) -> String {
        let rows = font::render("smeltery", BANNER_WIDTH - 2);
        let width = rows.iter().map(|r| r.chars().count()).max().unwrap_or(0);
        let mut out = String::from("\n");
        for row in &rows {
            out.push_str("  ");
            if self.styled {
                for (i, ch) in row.chars().enumerate() {
                    if ch == ' ' {
                        out.push(' ');
                    } else {
                        let _ = write!(out, "{}", ch.with(self.gradient(i, width)));
                    }
                }
            } else {
                out.push_str(row);
            }
            out.push('\n');
        }
        out.push_str("  ");
        let rule_width = width.max(TAGLINE.len() + version.len() + 4);
        for i in 0..rule_width.min(BANNER_WIDTH - 2) {
            if self.styled {
                let _ = write!(out, "{}", '─'.with(self.gradient(i, rule_width)));
            } else {
                out.push('─');
            }
        }
        out.push('\n');
        let subtitle = format!("{TAGLINE} · v{version}");
        let _ = writeln!(out, "  {}\n", self.dim(&subtitle));
        out
    }

    /// A question's title line and its dim key hint, after a divider when it is not the first one.
    pub(crate) fn section(self, step: usize, total: usize, title: &str, hint: &str) -> String {
        let mut out = String::new();
        if step > 1 {
            let _ = writeln!(out, "  {}", self.dim(&"─".repeat(56)));
        }
        let counter = if total > 0 {
            format!("  {step}/{total}")
        } else {
            String::new()
        };
        if self.styled {
            let _ = writeln!(
                out,
                "  {} {}{}",
                "◆".with(self.ember()),
                title.attribute(Attribute::Bold),
                counter.with(Color::DarkGrey)
            );
        } else {
            let _ = writeln!(out, "  * {title}{counter}");
        }
        let _ = writeln!(out, "    {}", self.dim(hint));
        out
    }

    /// A question shown as already answered (preview mode, where no prompt runs).
    pub(crate) fn answered(self, answer: &str) -> String {
        if self.styled {
            format!(
                "  {} {}\n",
                "✓".with(self.green()),
                answer.with(self.gold()).attribute(Attribute::Bold)
            )
        } else {
            format!("  > {answer}\n")
        }
    }

    /// A note under an answered question (a building block added because another needs it, or one that pairs well
    /// with a chosen block): `› text` in the question's indent, or `  note: text` without colour.
    pub(crate) fn note_line(self, text: &str) -> String {
        if self.styled {
            format!("    {} {}", "›".with(self.gold()), self.dim(text))
        } else {
            format!("  note: {text}")
        }
    }

    /// A badge: a coloured block with the padded label, or `[LABEL]` without colour.
    pub(crate) fn badge(self, badge: Badge) -> String {
        let label = format!(" {} ", badge.label());
        if self.styled {
            let (bg, fg) = if self.truecolor {
                badge.colors()
            } else {
                badge.ansi()
            };
            label.on(bg).with(fg).attribute(Attribute::Bold).to_string()
        } else {
            format!("[{}]", badge.label())
        }
    }

    /// A badge and a message on one line.
    pub(crate) fn badged(self, badge: Badge, message: &str) -> String {
        format!("{} {message}", self.badge(badge))
    }

    /// A green `✓` progress line.
    pub(crate) fn done_line(self, text: &str) -> String {
        if self.styled {
            format!(
                "  {} {text}",
                "✓".with(self.green()).attribute(Attribute::Bold)
            )
        } else {
            format!("  ok {text}")
        }
    }

    /// A red `✗` progress line with the reason.
    pub(crate) fn fail_line(self, text: &str, reason: &str) -> String {
        if self.styled {
            format!(
                "  {} {text} {}",
                "✗".with(self.red()).attribute(Attribute::Bold),
                format!("· {reason}").with(self.red())
            )
        } else {
            format!("  failed {text}: {reason}")
        }
    }

    /// Start a spinner for a long step. It animates only on a terminal; [`Spinner::finish`] clears it.
    pub(crate) fn spinner(self, text: &str) -> Spinner {
        let stop = Arc::new(AtomicBool::new(false));
        let handle = (self.styled && self.tty).then(|| {
            let stop = Arc::clone(&stop);
            let text = text.to_owned();
            let color = self.ember();
            std::thread::spawn(move || {
                const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
                let mut i = 0usize;
                while !stop.load(Ordering::Relaxed) {
                    let frame = FRAMES.get(i % FRAMES.len()).copied().unwrap_or('·');
                    let mut out = std::io::stdout().lock();
                    let _ = write!(out, "\r  {} {text}", frame.with(color));
                    let _ = out.flush();
                    drop(out);
                    i += 1;
                    std::thread::sleep(Duration::from_millis(80));
                }
            })
        });
        Spinner { stop, handle }
    }

    /// The final summary: a rounded box with the choices, the one-line command (highlighted) and next steps.
    pub(crate) fn summary_box(
        self,
        title: &str,
        rows: &[(&str, String)],
        command: &str,
        next: &[String],
    ) -> String {
        let inner = 72usize;
        let mut lines: Vec<(String, usize)> = Vec::new();
        let push = |lines: &mut Vec<(String, usize)>, styled: String, plain_len: usize| {
            lines.push((styled, plain_len));
        };
        let t = if self.styled {
            title
                .with(self.gold())
                .attribute(Attribute::Bold)
                .to_string()
        } else {
            title.to_owned()
        };
        push(&mut lines, t, title.chars().count());
        push(&mut lines, String::new(), 0);
        for (key, value) in rows {
            let plain = format!("{key:<10}{value}");
            let styled = format!("{}{value}", self.dim(&format!("{key:<10}")));
            push(&mut lines, styled, plain.chars().count());
        }
        push(&mut lines, String::new(), 0);
        let label = "Same app without questions:";
        push(&mut lines, self.dim(label), label.chars().count());
        for part in wrap_command(command, inner - 2) {
            let len = part.chars().count() + 2;
            let styled = if self.styled {
                format!("  {}", part.with(self.ember()).attribute(Attribute::Bold))
            } else {
                format!("  {part}")
            };
            push(&mut lines, styled, len);
        }
        push(&mut lines, String::new(), 0);
        let label = "Next steps:";
        push(&mut lines, self.dim(label), label.chars().count());
        for step in next {
            let styled = format!(
                "  {} {step}",
                if self.styled {
                    "›".with(self.ember()).to_string()
                } else {
                    ">".into()
                }
            );
            push(&mut lines, styled, step.chars().count() + 4);
        }
        let width = lines.iter().map(|(_, l)| *l).max().unwrap_or(0).min(inner);
        let border = |s: &str| {
            if self.styled {
                s.with(self.ember()).to_string()
            } else {
                s.to_owned()
            }
        };
        let mut out = String::new();
        let _ = writeln!(out, "  {}", border(&format!("╭{}╮", "─".repeat(width + 2))));
        for (styled, len) in &lines {
            let pad = width.saturating_sub(*len);
            let _ = writeln!(
                out,
                "  {} {styled}{} {}",
                border("│"),
                " ".repeat(pad),
                border("│")
            );
        }
        let _ = writeln!(out, "  {}", border(&format!("╰{}╯", "─".repeat(width + 2))));
        out
    }

    /// The inquire theme: coloured prompt prefix, `›` cursor, `●` / `○` checkboxes, coloured answers, dim help,
    /// styled errors. Without styling, inquire's colourless theme.
    pub(crate) fn render_config(self) -> inquire::ui::RenderConfig<'static> {
        use inquire::ui::{
            Attributes, Color as C, ErrorMessageRenderConfig, RenderConfig, StyleSheet, Styled,
        };
        if !self.styled {
            return RenderConfig::empty();
        }
        let (ember, gold, green, red) = if self.truecolor {
            (
                C::Rgb {
                    r: EMBER.0,
                    g: EMBER.1,
                    b: EMBER.2,
                },
                C::Rgb {
                    r: GOLD.0,
                    g: GOLD.1,
                    b: GOLD.2,
                },
                C::Rgb {
                    r: 74,
                    g: 222,
                    b: 128,
                },
                C::Rgb {
                    r: 248,
                    g: 113,
                    b: 113,
                },
            )
        } else {
            (
                C::AnsiValue(208),
                C::AnsiValue(220),
                C::AnsiValue(78),
                C::AnsiValue(203),
            )
        };
        RenderConfig::default_colored()
            .with_prompt_prefix(Styled::new("?").with_fg(ember).with_attr(Attributes::BOLD))
            .with_answered_prompt_prefix(
                Styled::new("✓").with_fg(green).with_attr(Attributes::BOLD),
            )
            .with_highlighted_option_prefix(
                Styled::new("›").with_fg(ember).with_attr(Attributes::BOLD),
            )
            .with_selected_checkbox(Styled::new("●").with_fg(green))
            .with_unselected_checkbox(Styled::new("○").with_fg(C::DarkGrey))
            .with_selected_option(Some(
                StyleSheet::new().with_fg(ember).with_attr(Attributes::BOLD),
            ))
            .with_answer(StyleSheet::new().with_fg(gold).with_attr(Attributes::BOLD))
            .with_help_message(StyleSheet::new().with_fg(C::DarkGrey))
            .with_error_message(
                ErrorMessageRenderConfig::default_colored()
                    .with_prefix(Styled::new("✗").with_fg(red).with_attr(Attributes::BOLD))
                    .with_message(StyleSheet::new().with_fg(red)),
            )
            .with_canceled_prompt_indicator(Styled::new("cancelled").with_fg(red))
    }
}

/// A running spinner; [`Spinner::finish`] stops it and clears its line.
pub(crate) struct Spinner {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Spinner {
    /// Stop the animation and clear its line, so the caller can print the result line.
    pub(crate) fn finish(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
            let mut out = std::io::stdout().lock();
            let _ = crossterm::execute!(
                out,
                crossterm::cursor::MoveToColumn(0),
                crossterm::terminal::Clear(crossterm::terminal::ClearType::CurrentLine)
            );
            let _ = out.flush();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        // A spinner dropped on an error path must not keep drawing over the error message.
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Split a shell command into lines of at most `width` columns, continued with ` \` so the wrapped text still
/// pastes as one command.
fn wrap_command(command: &str, width: usize) -> Vec<String> {
    let mut lines = vec![String::new()];
    for word in command.split(' ') {
        let current = lines.last().map_or(0, |l| l.chars().count());
        if current > 0 && current + 1 + word.len() + 2 > width {
            if let Some(last) = lines.last_mut() {
                last.push_str(" \\");
            }
            lines.push(format!("  {word}"));
        } else if let Some(last) = lines.last_mut() {
            if !last.is_empty() {
                last.push(' ');
            }
            last.push_str(word);
        }
    }
    lines
}

fn env_set(key: &str) -> bool {
    std::env::var_os(key).is_some_and(|v| !v.is_empty())
}

/// Whether the terminal understands ANSI escapes. Windows consoles need virtual terminal processing enabled,
/// which crossterm does; when that fails the CLI prints plain text.
fn ansi_ok() -> bool {
    #[cfg(windows)]
    {
        crossterm::ansi_support::supports_ansi()
    }
    #[cfg(not(windows))]
    {
        true
    }
}

/// Strip ANSI escape sequences (for width checks).
#[cfg(test)]
pub(crate) fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_fits_80_columns_styled_and_plain() {
        for ui in [Ui::plain(), Ui::styled_for_test()] {
            let banner = ui.banner("0.1.0");
            for line in strip_ansi(&banner).lines() {
                assert!(line.chars().count() <= BANNER_WIDTH, "{line:?}");
            }
        }
        let rows = strip_ansi(&Ui::plain().banner("0.1.0"));
        // The first glyph row of S, M, E, L, T, E, R, Y: full blocks, strokes two columns wide (D-235).
        assert!(
            rows.contains("███████ ███    ███ ███████ ██      ████████ ███████ ██████  ██    ██"),
            "{rows}"
        );
        assert!(!rows.contains('▀') && !rows.contains('▄'), "{rows}");
        assert!(rows.contains("Batteries-included full-stack Rust · v0.1.0"));
        assert!(rows.contains('█'));
    }

    #[test]
    fn styled_banner_uses_a_gradient() {
        let banner = Ui::styled_for_test().banner("0.1.0");
        assert!(banner.contains("\u{1b}[38;2;249;115;22m"), "starts ember");
        assert!(banner.contains("\u{1b}[38;2;250;204;21m"), "ends gold");
    }

    #[test]
    fn badges_without_colour_are_plain_labels() {
        let ui = Ui::plain();
        assert_eq!(ui.badge(Badge::Info), "[INFO]");
        assert_eq!(ui.badge(Badge::Warn), "[WARN]");
        assert_eq!(ui.badge(Badge::Done), "[DONE]");
        assert_eq!(ui.badge(Badge::Error), "[ERROR]");
        assert!(!ui.badged(Badge::Warn, "x").contains('\u{1b}'));
        let styled = Ui::styled_for_test().badge(Badge::Done);
        assert!(styled.contains("\u{1b}[") && strip_ansi(&styled) == " DONE ");
    }

    #[test]
    fn summary_box_is_rectangular_and_wraps_the_command() {
        let ui = Ui::styled_for_test();
        let command = "smeltery new my-app --kind web --db postgres --frontend mold --tailwind --no-alpine --smelt watchfire,temper --bellows mcp,skills,guidelines --no-migrate --no-seed --no-git";
        let text = strip_ansi(&ui.summary_box(
            "Created my-app",
            &[("kind", "web".into()), ("database", "postgres".into())],
            command,
            &["cd my-app && smeltery serve".into()],
        ));
        let widths: Vec<usize> = text.lines().map(|l| l.chars().count()).collect();
        assert!(widths.windows(2).all(|w| w[0] == w[1]), "{text}");
        assert!(widths[0] <= 80);
        let rejoined: String = text
            .lines()
            .filter(|l| l.contains("smeltery new") || l.contains("--"))
            .map(|l| {
                l.trim_matches(|c| c == '│' || c == ' ')
                    .trim_end_matches(" \\")
                    .trim()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(rejoined, command);
    }

    #[test]
    fn plain_helpers_have_no_escapes() {
        let ui = Ui::plain();
        for s in [
            ui.section(2, 7, "Database", "↑↓ move · enter confirm"),
            ui.answered("SQLite"),
            ui.done_line("app/"),
            ui.fail_line("migrate", "exit 1"),
            ui.note_line("Hallmark needs Temper: added"),
            ui.summary_box(
                "Created x",
                &[("kind", "web".into())],
                "smeltery new x",
                &["cd x".into()],
            ),
        ] {
            assert!(!s.contains('\u{1b}'), "{s}");
        }
    }

    #[test]
    fn note_lines_say_the_same_with_and_without_colour() {
        let text = "Anvil works with Temper: private channels";
        assert_eq!(Ui::plain().note_line(text), format!("  note: {text}"));
        let styled = Ui::styled_for_test().note_line(text);
        assert!(styled.contains("\u{1b}["));
        assert_eq!(strip_ansi(&styled), format!("    › {text}"));
    }
}
