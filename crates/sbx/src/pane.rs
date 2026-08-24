//! Markup shared by the panes that fetch a body of text.
//!
//! The diff and policy panes both build their content as plain text and let the
//! renderer colour it, rather than constructing styled lines at the point of
//! fetch. That keeps the fetching code testable without a terminal, and keeps
//! every styling decision in one file.
//!
//! The sigils are chosen so they cannot collide with the content they wrap.
//! Unified diff output can never produce a line starting with `#` or `!` in
//! column zero: body lines always begin with `+`, `-`, ` `, `@` or `\`, and file
//! headers with `diff`/`index`. They are a contract with
//! [`crate::tui::ui`], which strips them.

/// A heading.
pub const SECTION: &str = "### ";
/// Something the user needs to know about the content: a truncation, a
/// caveat, a value that could not be resolved.
pub const NOTICE: &str = "!!! ";
/// A `label  value` row. Rendered with the label dimmed.
pub const FIELD: &str = "::: ";

/// Emit a heading line.
pub fn section(out: &mut String, title: impl std::fmt::Display) {
    out.push_str(SECTION);
    out.push_str(&title.to_string());
    out.push('\n');
}

/// Emit a notice line.
pub fn notice(out: &mut String, text: impl std::fmt::Display) {
    out.push_str(NOTICE);
    out.push_str(&text.to_string());
    out.push('\n');
}

/// Emit a `label  value` row. The label is padded here rather than at render
/// time so the alignment is visible in the tests and in `sbx policy` output.
pub fn field(out: &mut String, label: &str, value: impl std::fmt::Display) {
    out.push_str(FIELD);
    out.push_str(&format!("{label:<12}{value}\n"));
}

/// Strip the sigils for output that is read rather than rendered: `sbx policy`
/// prints to a pipe, where a terminal's colours are not available and `### `
/// would be noise.
pub fn to_plain(body: &str) -> String {
    let mut out = String::new();
    for line in body.lines() {
        if let Some(heading) = line.strip_prefix(SECTION) {
            out.push_str(&format!("\n{heading}\n"));
        } else if let Some(notice) = line.strip_prefix(NOTICE) {
            out.push_str(&format!("  ! {notice}\n"));
        } else if let Some(field) = line.strip_prefix(FIELD) {
            out.push_str(&format!("  {field}\n"));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    // The leading blank a first heading introduces is wrong at the top.
    out.trim_start_matches('\n').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sigils are a contract with the renderer, which strips them. If they
    /// drift, the panes show raw markers.
    #[test]
    fn the_sigils_are_what_the_renderer_strips() {
        assert_eq!(SECTION, "### ");
        assert_eq!(NOTICE, "!!! ");
        assert_eq!(FIELD, "::: ");
        // All the same width, so a body can be scanned in a debugger without
        // the columns jumping.
        assert_eq!(NOTICE.len(), SECTION.len());
        assert_eq!(FIELD.len(), SECTION.len());
    }

    /// None of them may collide with a line unified diff output can produce,
    /// since the diff pane shares this vocabulary.
    #[test]
    fn no_sigil_can_be_confused_with_diff_content() {
        for sigil in [SECTION, NOTICE, FIELD] {
            let first = sigil.chars().next().unwrap();
            assert!(
                !"+- @\\".contains(first),
                "{sigil} starts with a diff body character"
            );
            assert!(!"di".contains(first), "{sigil} could start diff/index");
        }
    }

    #[test]
    fn plain_output_carries_no_sigils() {
        let mut body = String::new();
        section(&mut body, "network");
        field(&mut body, "host", "github.com:443");
        notice(&mut body, "truncated");
        body.push_str("bare line\n");

        let plain = to_plain(&body);
        for sigil in [SECTION, NOTICE, FIELD] {
            assert!(!plain.contains(sigil), "{sigil} survived: {plain:?}");
        }
        assert!(
            plain.starts_with("network\n"),
            "no leading blank: {plain:?}"
        );
        assert!(
            plain.contains("  host        github.com:443\n"),
            "{plain:?}"
        );
        assert!(plain.contains("  ! truncated\n"), "{plain:?}");
        assert!(plain.contains("bare line\n"), "{plain:?}");
    }

    #[test]
    fn emits_one_line_per_call() {
        let mut out = String::new();
        section(&mut out, "network");
        field(&mut out, "host", "github.com:443");
        notice(&mut out, "truncated");
        assert_eq!(
            out,
            "### network\n::: host        github.com:443\n!!! truncated\n"
        );
    }
}
