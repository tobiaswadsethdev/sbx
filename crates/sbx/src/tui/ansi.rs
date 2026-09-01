//! The captured screen, in ratatui's vocabulary.
//!
//! [`sbx_core::ansi`] tokenizes a `capture-pane -e` line into runs carrying its own
//! style type, because the same screen has more than one renderer now and a
//! tokenizer that spoke ratatui could only ever have one. This is the half that
//! knows about ratatui, and it is a mapping and nothing else: no parsing lives
//! here, so a sequence this draws wrongly is a bug in one place rather than two.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use sbx_core::ansi;

/// One captured line, as spans carrying the colour it was drawn with.
pub fn to_line(line: &str) -> Line<'static> {
    let spans: Vec<Span<'static>> = ansi::spans(line)
        .into_iter()
        .map(|s| Span::styled(s.text, style(s.style)))
        .collect();
    Line::from(spans)
}

fn style(s: ansi::Style) -> Style {
    let mut out = Style::default();
    if let Some(fg) = s.fg {
        out = out.fg(colour(fg));
    }
    if let Some(bg) = s.bg {
        out = out.bg(colour(bg));
    }
    if !s.modifiers.is_empty() {
        out = out.add_modifier(modifiers(s.modifiers));
    }
    out
}

fn modifiers(m: ansi::Modifiers) -> Modifier {
    let mut out = Modifier::empty();
    for (ours, theirs) in [
        (ansi::Modifiers::BOLD, Modifier::BOLD),
        (ansi::Modifiers::DIM, Modifier::DIM),
        (ansi::Modifiers::ITALIC, Modifier::ITALIC),
        (ansi::Modifiers::UNDERLINED, Modifier::UNDERLINED),
        (ansi::Modifiers::SLOW_BLINK, Modifier::SLOW_BLINK),
        (ansi::Modifiers::REVERSED, Modifier::REVERSED),
        (ansi::Modifiers::CROSSED_OUT, Modifier::CROSSED_OUT),
    ] {
        if m.contains(ours) {
            out |= theirs;
        }
    }
    out
}

fn colour(c: ansi::Color) -> Color {
    match c {
        ansi::Color::Black => Color::Black,
        ansi::Color::Red => Color::Red,
        ansi::Color::Green => Color::Green,
        ansi::Color::Yellow => Color::Yellow,
        ansi::Color::Blue => Color::Blue,
        ansi::Color::Magenta => Color::Magenta,
        ansi::Color::Cyan => Color::Cyan,
        ansi::Color::Gray => Color::Gray,
        ansi::Color::DarkGray => Color::DarkGray,
        ansi::Color::LightRed => Color::LightRed,
        ansi::Color::LightGreen => Color::LightGreen,
        ansi::Color::LightYellow => Color::LightYellow,
        ansi::Color::LightBlue => Color::LightBlue,
        ansi::Color::LightMagenta => Color::LightMagenta,
        ansi::Color::LightCyan => Color::LightCyan,
        ansi::Color::White => Color::White,
        ansi::Color::Indexed(n) => Color::Indexed(n),
        ansi::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
        ansi::Color::Reset => Color::Reset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.to_string()).collect()
    }

    #[test]
    fn colours_and_text_survive_the_mapping() {
        let line = to_line("\x1b[31mred\x1b[0m plain \x1b[38;5;208mindexed\x1b[0m");
        assert_eq!(text_of(&line), "red plain indexed");

        let fg: Vec<Option<Color>> = line.spans.iter().map(|s| s.style.fg).collect();
        assert_eq!(fg[0], Some(Color::Red));
        // Unset stays unset, so the pane's own colours show through rather than
        // having the terminal default painted over them.
        assert_eq!(fg[1], None);
        assert_eq!(fg[2], Some(Color::Indexed(208)));
    }

    #[test]
    fn truecolor_backgrounds_and_modifiers_survive_the_mapping() {
        let line = to_line("\x1b[1;4;38;2;255;128;0;48;5;236mwarm");
        let style = line.spans[0].style;
        assert_eq!(style.fg, Some(Color::Rgb(255, 128, 0)));
        assert_eq!(style.bg, Some(Color::Indexed(236)));
        assert!(style.add_modifier.contains(Modifier::BOLD));
        assert!(style.add_modifier.contains(Modifier::UNDERLINED));
    }

    /// Every modifier the tokenizer can set has somewhere to land. A missing arm
    /// would silently drop an attribute rather than fail to compile.
    #[test]
    fn every_modifier_maps_to_one_of_ratatuis() {
        for ours in [
            ansi::Modifiers::BOLD,
            ansi::Modifiers::DIM,
            ansi::Modifiers::ITALIC,
            ansi::Modifiers::UNDERLINED,
            ansi::Modifiers::SLOW_BLINK,
            ansi::Modifiers::REVERSED,
            ansi::Modifiers::CROSSED_OUT,
        ] {
            assert!(
                !modifiers(ours).is_empty(),
                "{ours:?} maps to nothing at all"
            );
        }
    }

    #[test]
    fn a_reset_is_ratatuis_reset_not_a_colour() {
        let line = to_line("\x1b[32mgreen\x1b[39mplain");
        assert_eq!(line.spans[1].style.fg, Some(Color::Reset));
    }
}
