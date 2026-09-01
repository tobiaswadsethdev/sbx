//! Terminal escape sequences: enough of them to redraw a captured screen.
//!
//! The agent's screen is captured with `capture-pane -e`, which keeps the colour
//! it was drawn with. Two things need that text: the pane that shows it, which
//! wants the colour, and [`crate::status`], which matches markers in it and must
//! not have an escape sequence land in the middle of a phrase it is looking for.
//! Both come out of the same tokenizer here -- [`spans`] and [`strip`] -- so
//! they can never disagree about where the text is.
//!
//! Deliberately not a terminal emulator. There is no cursor, no scroll region and
//! no character set switching: the capture is already a laid-out screen, one line
//! per line, so the only sequences that carry meaning are the colour ones. Every
//! other escape is skipped rather than guessed at, which is the difference
//! between this and a dependency.
//!
//! The style types here are this crate's own rather than a renderer's. They were
//! ratatui's until the core was pulled out from under the TUI: a tokenizer that
//! speaks in one renderer's vocabulary cannot be used by a second one, and this
//! is the same screen whether it is drawn into a terminal or sent to a client
//! that will draw it somewhere else. The mapping to ratatui now lives with the
//! TUI, which is the only place that needs it.

use serde::{Deserialize, Serialize};

/// A colour a captured screen can carry.
///
/// The eight base colours are named rather than indexed so the *renderer's*
/// theme decides what they look like -- the same reason the rest of the
/// interface uses names. [`Color::Reset`] is "whatever was there before this
/// text", which is not the same as black.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Color {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    Gray,
    DarkGray,
    LightRed,
    LightGreen,
    LightYellow,
    LightBlue,
    LightMagenta,
    LightCyan,
    White,
    /// One of the 256 palette entries.
    Indexed(u8),
    /// Direct colour.
    Rgb(u8, u8, u8),
    /// Back to the surface's own colour.
    Reset,
}

/// The attributes a run of text can carry, as a set.
///
/// A bitset rather than a struct of booleans because SGR turns several of them
/// off together -- `22` clears bold *and* dim -- and that reads as one operation
/// here rather than two assignments that could drift apart.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Modifiers(u16);

impl Modifiers {
    pub const BOLD: Self = Self(1 << 0);
    pub const DIM: Self = Self(1 << 1);
    pub const ITALIC: Self = Self(1 << 2);
    pub const UNDERLINED: Self = Self(1 << 3);
    pub const SLOW_BLINK: Self = Self(1 << 4);
    pub const REVERSED: Self = Self(1 << 5);
    pub const CROSSED_OUT: Self = Self(1 << 6);

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for Modifiers {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// The style in force over a run of text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub modifiers: Modifiers,
}

impl Style {
    fn fg(mut self, colour: Color) -> Self {
        self.fg = Some(colour);
        self
    }

    fn bg(mut self, colour: Color) -> Self {
        self.bg = Some(colour);
        self
    }

    fn add_modifier(mut self, m: Modifiers) -> Self {
        self.modifiers = Modifiers(self.modifiers.0 | m.0);
        self
    }

    fn remove_modifier(mut self, m: Modifiers) -> Self {
        self.modifiers = Modifiers(self.modifiers.0 & !m.0);
        self
    }
}

/// A run of text and the style in force where it starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub style: Style,
    pub text: String,
}

/// Split a line into runs of text, each with the style in force where it starts.
///
/// The caller decides what to do with them: the TUI turns them into ratatui
/// spans and [`strip`] throws the styles away.
pub fn spans(line: &str) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut style = Style::default();
    let mut text = String::new();
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '\x1b' {
            text.push(c);
            continue;
        }
        // An escape ends the run before it, whatever it turns out to be: a
        // sequence in the middle of a word still splits the word's styling.
        match chars.peek() {
            // CSI: parameters, then a final byte that says what to do with them.
            Some('[') => {
                chars.next();
                let mut params = String::new();
                let mut final_byte = None;
                for c in chars.by_ref() {
                    if c.is_ascii_digit() || c == ';' || c == ':' || c == '?' {
                        params.push(c);
                    } else {
                        final_byte = Some(c);
                        break;
                    }
                }
                // `m` is the one that matters. Cursor moves and erases cannot
                // apply to a screen that has already been laid out, so they are
                // dropped rather than interpreted.
                if final_byte == Some('m') {
                    if !text.is_empty() {
                        out.push(Span {
                            style,
                            text: std::mem::take(&mut text),
                        });
                    }
                    style = apply_sgr(style, &params);
                }
            }
            // OSC: runs until a bell or a string terminator. Titles, hyperlinks --
            // nothing that shows up in a captured pane's text.
            Some(']') => {
                chars.next();
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            // Two-character escapes: charset selection, keypad modes, and so on.
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }

    if !text.is_empty() {
        out.push(Span { style, text });
    }
    out
}

/// The visible text of a line, with every escape sequence removed.
///
/// What [`crate::status::scrape_pane`] matches against: `esc to interrupt` is not
/// findable in a string where tmux has coloured `esc` separately.
pub fn strip(text: &str) -> String {
    text.lines()
        .map(|line| spans(line).into_iter().map(|s| s.text).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Apply one `m` sequence's parameters to the style in force.
///
/// Unknown parameters are skipped rather than treated as a reset: a sequence this
/// does not model should cost the colour it carries, not the colour already on
/// screen.
fn apply_sgr(mut style: Style, params: &str) -> Style {
    // A bare `\x1b[m` means reset, the same as `\x1b[0m`.
    if params.is_empty() {
        return Style::default();
    }
    // `:` separates the parts of one parameter (`38:2:r:g:b`) and `;` separates
    // parameters; for the ones here both can be read the same way.
    let mut it = params.split([';', ':']).peekable();

    while let Some(part) = it.next() {
        let Ok(code) = part.parse::<u8>() else {
            continue;
        };
        match code {
            0 => style = Style::default(),
            1 => style = style.add_modifier(Modifiers::BOLD),
            2 => style = style.add_modifier(Modifiers::DIM),
            3 => style = style.add_modifier(Modifiers::ITALIC),
            4 => style = style.add_modifier(Modifiers::UNDERLINED),
            5 => style = style.add_modifier(Modifiers::SLOW_BLINK),
            7 => style = style.add_modifier(Modifiers::REVERSED),
            9 => style = style.add_modifier(Modifiers::CROSSED_OUT),
            22 => style = style.remove_modifier(Modifiers::BOLD | Modifiers::DIM),
            23 => style = style.remove_modifier(Modifiers::ITALIC),
            24 => style = style.remove_modifier(Modifiers::UNDERLINED),
            27 => style = style.remove_modifier(Modifiers::REVERSED),
            30..=37 => style = style.fg(ansi16(code - 30)),
            39 => style = style.fg(Color::Reset),
            40..=47 => style = style.bg(ansi16(code - 40)),
            49 => style = style.bg(Color::Reset),
            90..=97 => style = style.fg(bright16(code - 90)),
            100..=107 => style = style.bg(bright16(code - 100)),
            // Extended colour: `38;5;n` for indexed, `38;2;r;g;b` for direct.
            38 | 48 => {
                let Some(colour) = extended(&mut it) else {
                    // Malformed: the rest of the parameters belong to a sequence
                    // this cannot make sense of, so stop rather than misread
                    // them as codes of their own.
                    break;
                };
                style = if code == 38 {
                    style.fg(colour)
                } else {
                    style.bg(colour)
                };
            }
            _ => {}
        }
    }
    style
}

/// Read the colour after a `38`/`48`, whichever form it takes.
///
/// Empty parts are skipped, because the colon form carries a colour-space field
/// that is almost always left blank -- `38:2::255:128:0` is what tmux emits, and
/// reading that blank as the red channel loses the colour entirely.
fn extended<'a>(it: &mut impl Iterator<Item = &'a str>) -> Option<Color> {
    let mut next = || loop {
        match it.next() {
            Some("") => continue,
            other => return other,
        }
    };
    match next()?.parse::<u8>().ok()? {
        5 => Some(Color::Indexed(next()?.parse().ok()?)),
        2 => {
            let r = next()?.parse().ok()?;
            let g = next()?.parse().ok()?;
            let b = next()?.parse().ok()?;
            Some(Color::Rgb(r, g, b))
        }
        _ => None,
    }
}

/// The eight ANSI colours, by name.
fn ansi16(n: u8) -> Color {
    match n {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        _ => Color::Gray,
    }
}

fn bright16(n: u8) -> Color {
    match n {
        0 => Color::DarkGray,
        1 => Color::LightRed,
        2 => Color::LightGreen,
        3 => Color::LightYellow,
        4 => Color::LightBlue,
        5 => Color::LightMagenta,
        6 => Color::LightCyan,
        _ => Color::White,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(spans: &[Span]) -> String {
        spans.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn plain_text_survives_unchanged() {
        assert_eq!(strip("hello"), "hello");
        assert_eq!(text_of(&spans("hello")), "hello");
        assert_eq!(spans("hello").len(), 1);
    }

    /// The reason `strip` exists: status detection matches phrases, and a phrase
    /// with a colour change inside it is not findable in the raw capture.
    #[test]
    fn a_sequence_inside_a_phrase_does_not_break_the_phrase() {
        let raw = "  \x1b[2mesc\x1b[0m to interrupt";
        assert_eq!(strip(raw), "  esc to interrupt");
        assert!(strip(raw).contains("esc to interrupt"));
    }

    #[test]
    fn colours_become_spans() {
        let line = spans("\x1b[31mred\x1b[0m plain \x1b[38;5;208mindexed\x1b[0m");
        assert_eq!(text_of(&line), "red plain indexed");

        let styled: Vec<(&str, Option<Color>)> =
            line.iter().map(|s| (s.text.as_str(), s.style.fg)).collect();
        assert_eq!(styled[0], ("red", Some(Color::Red)));
        // `0m` is a reset to *nothing set*, which is what lets the pane's own
        // colours show through rather than painting the terminal default over
        // them.
        assert_eq!(styled[1], (" plain ", None));
        assert_eq!(styled[2], ("indexed", Some(Color::Indexed(208))));
    }

    #[test]
    fn truecolor_and_backgrounds_and_modifiers() {
        let line = spans("\x1b[1;38;2;255;128;0;48;5;236mwarm\x1b[0m");
        let span = &line[0];
        assert_eq!(span.style.fg, Some(Color::Rgb(255, 128, 0)));
        assert_eq!(span.style.bg, Some(Color::Indexed(236)));
        assert!(span.style.modifiers.contains(Modifiers::BOLD));
    }

    /// `22` turns off two attributes at once, which is the reason the set is a
    /// bitset rather than a field each.
    #[test]
    fn one_code_can_clear_two_attributes() {
        let line = spans("\x1b[1;2;3mloud\x1b[22mstill-italic");
        assert!(line[0].style.modifiers.contains(Modifiers::BOLD));
        assert!(line[0].style.modifiers.contains(Modifiers::DIM));
        assert!(!line[1].style.modifiers.contains(Modifiers::BOLD));
        assert!(!line[1].style.modifiers.contains(Modifiers::DIM));
        assert!(
            line[1].style.modifiers.contains(Modifiers::ITALIC),
            "22 clears bold and dim and nothing else"
        );
    }

    /// tmux writes the colon form for direct colour, and Claude Code's own
    /// output goes through tmux, so both forms have to work.
    #[test]
    fn the_colon_form_of_direct_colour_works_too() {
        let line = spans("\x1b[38:2::255:128:0mwarm");
        assert_eq!(line[0].style.fg, Some(Color::Rgb(255, 128, 0)));
        assert_eq!(text_of(&line), "warm");
    }

    /// Only `m` carries style. A cursor move applied to an already-laid-out
    /// screen would move nothing and drop a `2J` into the text if mishandled.
    #[test]
    fn other_sequences_are_dropped_not_printed() {
        for raw in [
            "\x1b[2Jcleared",
            "\x1b[1;1Hhome",
            "\x1b[?25lhidden",
            "\x1b]0;a title\x07titled",
            "\x1b]8;;https://example.com\x1b\\linked",
            "\x1b(Bcharset",
        ] {
            let out = strip(raw);
            assert!(
                !out.contains('\x1b') && !out.contains('['),
                "`{raw:?}` left `{out}`"
            );
        }
        assert_eq!(strip("\x1b[2Jcleared"), "cleared");
        assert_eq!(strip("\x1b]0;a title\x07titled"), "titled");
    }

    /// A style that is never reset applies to the rest of the line, and a reset
    /// mid-line ends it -- the two halves of getting a screen back.
    #[test]
    fn style_carries_forward_until_it_is_changed() {
        let line = spans("\x1b[32mgreen still-green\x1b[39mplain");
        assert_eq!(line[0].style.fg, Some(Color::Green));
        assert_eq!(line[0].text, "green still-green");
        assert_eq!(line[1].style.fg, Some(Color::Reset));
    }

    #[test]
    fn a_bare_reset_is_a_reset() {
        let line = spans("\x1b[1;31mloud\x1b[mquiet");
        assert!(line[0].style.modifiers.contains(Modifiers::BOLD));
        assert_eq!(line[1].style, Style::default());
    }

    /// Every line of a capture is stripped independently, and the line structure
    /// is what `squeeze` and the marker search both walk.
    #[test]
    fn strip_keeps_the_lines() {
        let raw = "\x1b[31mone\x1b[0m\n\x1b[32mtwo\x1b[0m\n\nfour";
        assert_eq!(strip(raw), "one\ntwo\n\nfour");
    }

    /// Real output, from `capture-pane -pe` against a live agent.
    #[test]
    fn a_real_captured_line_reads_back_as_its_text() {
        let raw = "\u{1b}[38;5;246m  \u{1b}[39m\u{1b}[38;5;246m⏸ manual mode on\u{1b}[39m\
                   \u{1b}[38;5;246m · \u{1b}[39m\u{1b}[38;5;246m? for shortcuts\u{1b}[39m";
        assert_eq!(strip(raw), "  ⏸ manual mode on · ? for shortcuts");
        let line = spans(raw);
        assert_eq!(text_of(&line), "  ⏸ manual mode on · ? for shortcuts");
        assert!(
            line.iter().any(|s| s.style.fg == Some(Color::Indexed(246))),
            "the grey it was drawn in survives"
        );
    }
}
