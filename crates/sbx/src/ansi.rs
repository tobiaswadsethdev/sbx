//! Terminal escape sequences: enough of them to redraw a captured screen.
//!
//! The agent's screen is captured with `capture-pane -e`, which keeps the colour
//! it was drawn with. Two things need that text: the pane that shows it, which
//! wants the colour, and [`crate::status`], which matches markers in it and must
//! not have an escape sequence land in the middle of a phrase it is looking for.
//! Both come out of the same tokenizer here -- [`to_line`] and [`strip`] -- so
//! they can never disagree about where the text is.
//!
//! Deliberately not a terminal emulator. There is no cursor, no scroll region and
//! no character set switching: the capture is already a laid-out screen, one line
//! per line, so the only sequences that carry meaning are the colour ones. Every
//! other escape is skipped rather than guessed at, which is the difference
//! between this and a dependency.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Split a line into runs of text, each with the style in force where it starts.
///
/// The caller decides what to do with them; [`to_line`] draws them and
/// [`strip`] throws the styles away.
fn runs(line: &str) -> Vec<(Style, String)> {
    let mut out: Vec<(Style, String)> = Vec::new();
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
                        out.push((style, std::mem::take(&mut text)));
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
        out.push((style, text));
    }
    out
}

/// The visible text of a line, with every escape sequence removed.
///
/// What [`crate::status::scrape_pane`] matches against: `esc to interrupt` is not
/// findable in a string where tmux has coloured `esc` separately.
pub fn strip(text: &str) -> String {
    text.lines()
        .map(|line| runs(line).into_iter().map(|(_, t)| t).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// One captured line, as spans carrying the colour it was drawn with.
pub fn to_line(line: &str) -> Line<'static> {
    let spans: Vec<Span<'static>> = runs(line)
        .into_iter()
        .map(|(style, text)| Span::styled(text, style))
        .collect();
    Line::from(spans)
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
            1 => style = style.add_modifier(Modifier::BOLD),
            2 => style = style.add_modifier(Modifier::DIM),
            3 => style = style.add_modifier(Modifier::ITALIC),
            4 => style = style.add_modifier(Modifier::UNDERLINED),
            5 => style = style.add_modifier(Modifier::SLOW_BLINK),
            7 => style = style.add_modifier(Modifier::REVERSED),
            9 => style = style.add_modifier(Modifier::CROSSED_OUT),
            22 => style = style.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => style = style.remove_modifier(Modifier::ITALIC),
            24 => style = style.remove_modifier(Modifier::UNDERLINED),
            27 => style = style.remove_modifier(Modifier::REVERSED),
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

/// The eight ANSI colours, as ratatui names them. Names rather than indices, so
/// the user's terminal theme decides what they look like -- the same reason the
/// rest of the interface uses them.
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

    fn text_of(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.to_string()).collect()
    }

    #[test]
    fn plain_text_survives_unchanged() {
        assert_eq!(strip("hello"), "hello");
        assert_eq!(text_of(&to_line("hello")), "hello");
        assert_eq!(to_line("hello").spans.len(), 1);
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
        let line = to_line("\x1b[31mred\x1b[0m plain \x1b[38;5;208mindexed\x1b[0m");
        assert_eq!(text_of(&line), "red plain indexed");

        let styled: Vec<(&str, Option<Color>)> = line
            .spans
            .iter()
            .map(|s| (s.content.as_ref(), s.style.fg))
            .collect();
        assert_eq!(styled[0], ("red", Some(Color::Red)));
        // `0m` is a reset to *nothing set*, which is what lets the pane's own
        // colours show through rather than painting the terminal default over
        // them.
        assert_eq!(styled[1], (" plain ", None));
        assert_eq!(styled[2], ("indexed", Some(Color::Indexed(208))));
    }

    #[test]
    fn truecolor_and_backgrounds_and_modifiers() {
        let line = to_line("\x1b[1;38;2;255;128;0;48;5;236mwarm\x1b[0m");
        let span = &line.spans[0];
        assert_eq!(span.style.fg, Some(Color::Rgb(255, 128, 0)));
        assert_eq!(span.style.bg, Some(Color::Indexed(236)));
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
    }

    /// tmux writes the colon form for direct colour, and Claude Code's own
    /// output goes through tmux, so both forms have to work.
    #[test]
    fn the_colon_form_of_direct_colour_works_too() {
        let line = to_line("\x1b[38:2::255:128:0mwarm");
        assert_eq!(line.spans[0].style.fg, Some(Color::Rgb(255, 128, 0)));
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
        let line = to_line("\x1b[32mgreen still-green\x1b[39mplain");
        assert_eq!(line.spans[0].style.fg, Some(Color::Green));
        assert_eq!(line.spans[0].content.as_ref(), "green still-green");
        assert_eq!(line.spans[1].style.fg, Some(Color::Reset));
    }

    #[test]
    fn a_bare_reset_is_a_reset() {
        let line = to_line("\x1b[1;31mloud\x1b[mquiet");
        assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(line.spans[1].style, Style::default());
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
        let line = to_line(raw);
        assert_eq!(text_of(&line), "  ⏸ manual mode on · ? for shortcuts");
        assert!(
            line.spans
                .iter()
                .any(|s| s.style.fg == Some(Color::Indexed(246))),
            "the grey it was drawn in survives"
        );
    }
}
