//! Working out what an agent is actually doing.
//!
//! Two sources, because neither is sufficient alone.
//!
//! **Hooks.** The image bakes Claude Code hooks that write
//! `/sandbox/.sbx/status.json` on turn and tool boundaries. That gives clean
//! turn edges -- `UserPromptSubmit` starts one, `Stop` ends it -- and a tool
//! name to display, with a timestamp that says whether the agent is still
//! alive.
//!
//! **The pane.** `tmux capture-pane` against the agent's session, matched
//! against markers taken from real specimens in `tests/panes`.
//!
//! The plan expected the pane to be a fallback for agents without hooks. It is
//! the other way round, and both halves of that were found by watching a real
//! session rather than by reasoning about it. Measured against Claude Code
//! 2.1.143:
//!
//! * **No `Notification` for a permission prompt.** A sandbox sitting on "Do
//!   you want to proceed?" reports `{"state":"running","detail":"Bash"}` from
//!   `PreToolUse` and stays there indefinitely.
//! * **No `Stop` for an interrupt.** Pressing Escape returns the agent to its
//!   input box without ending a turn, so the file keeps saying `running` while
//!   the screen plainly shows an idle prompt.
//!
//! Both are the same failure: hooks report *events*, and there is no event for
//! every state an agent can be in. The pane is a direct observation of the
//! screen, so it decides, and the file supplies what the screen does not say
//! cleanly -- the name of the tool in play -- plus an answer for a sandbox with
//! no agent pane to read at all.
//!
//! **What 2.1.246 changed.** The footer under the input box is no longer a
//! fixed hint. It is a list -- permission mode, then a rotating tip, then
//! whatever else applies -- truncated with an ellipsis to the pane width, and
//! `? for shortcuts` is only one of the tips that slot can hold. An idle agent
//! therefore often shows no idle marker at all, which is why the input box is
//! now recognised by its shape rather than by the words under it. `esc to
//! interrupt` survived, as an entry in the same list; it is checked first, so a
//! running agent is never read as idle merely for having an input box on screen.
//! The specimens for both are committed next to the older ones.

use serde::Deserialize;

use crate::session::State;

/// Separates the status file from the pane in the poll script's output.
pub const STATUS_MARKER: &str = "===sbx-status===";
pub const PANE_MARKER: &str = "===sbx-pane===";

/// How long the hook file is trusted when there is no pane to read, in seconds.
///
/// Only reached for a sandbox whose agent was never started or whose tmux
/// session is gone, since otherwise the screen decides. Generous even so: a
/// single long-running tool fires `PreToolUse` and then nothing until it
/// finishes, so a quiet file does not mean a dead agent.
const HOOK_STALE_SECS: u64 = 120;

/// Footer shown only while a prompt is waiting to be answered. Absent from both
/// the idle and the working specimens, which makes it the single most reliable
/// marker.
const WAITING_FOOTER: &str = "Esc to cancel";
/// Shown while the agent is working.
const RUNNING_HINT: &str = "esc to interrupt";
/// One of the tips that can appear under the input box. Present in 2.1.143's
/// idle screen, and still a valid signal, but by 2.1.246 it shares that slot
/// with several others -- so its absence means nothing. See [`has_input_box`].
const IDLE_HINT: &str = "? for shortcuts";
/// The rule drawn above and below the input box. A different character from the
/// dashed rule (`╌`) an edit confirmation draws, which is what keeps a permission
/// prompt from looking like an input box.
const BOX_RULE: char = '─';
/// How long a run of [`BOX_RULE`] counts as the box. Long enough that a rule an
/// agent happens to print inside its own output is not mistaken for one.
const BOX_RULE_MIN: usize = 20;
/// How many lines may sit between the two rules. The prompt wraps, so the box
/// grows with what has been typed into it.
const BOX_MAX_HEIGHT: usize = 8;
/// Selection cursor. On its own this means nothing: the idle input box uses the
/// same glyph (`❯ commit this`). Only a cursor sitting on a *numbered option*
/// indicates an open menu.
const CURSOR: char = '❯';

/// Where a report came from. Kept because "the pane says waiting but the hooks
/// say running" is the normal case, not an error, and being able to see which
/// one won makes the difference between a bug and a misunderstanding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Hook,
    Pane,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub state: State,
    /// Tool name or prompt message, when one is known.
    pub detail: Option<String>,
    pub source: Source,
}

/// The record `sbx-status` writes inside the sandbox.
///
/// Field names are a contract with `images/sbx-base/sbx-status`; `image.rs` has
/// a test asserting the script still writes them.
#[derive(Debug, Clone, Deserialize)]
pub struct HookStatus {
    pub state: String,
    /// Epoch seconds, from `date +%s` inside the sandbox.
    pub at: u64,
    #[serde(default)]
    pub detail: String,
}

/// What the agent's screen says it is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneSignal {
    Waiting,
    Running,
    Idle,
}

pub fn parse_hook(json: &str) -> Option<HookStatus> {
    let json = json.trim();
    if json.is_empty() {
        return None;
    }
    serde_json::from_str(json).ok()
}

/// A cursor sitting on a numbered option, e.g. `❯ 1. Yes`.
///
/// Matched structurally rather than by question text: the two real specimens
/// ask "Do you want to make this edit to README.md?" and "Do you want to
/// proceed?", so the wording varies with the tool while the menu does not.
fn has_numbered_cursor(pane: &str) -> bool {
    pane.lines().any(|line| {
        let Some((_, after)) = line.split_once(CURSOR) else {
            return false;
        };
        let after = after.trim_start();
        let digits = after.chars().take_while(char::is_ascii_digit).count();
        digits > 0 && after[digits..].starts_with('.')
    })
}

/// Whether the agent's input box is on screen: two rules with the prompt
/// between them.
///
/// Recognised by shape because the words under the box are not dependable -- see
/// the module comment. Only ever consulted after the waiting and running
/// markers, since a permission prompt is drawn over the box and a working agent
/// keeps it on screen.
fn has_input_box(pane: &str) -> bool {
    let lines: Vec<&str> = pane.lines().collect();
    let rules: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.chars().filter(|c| *c == BOX_RULE).count() >= BOX_RULE_MIN)
        .map(|(i, _)| i)
        .collect();

    rules.windows(2).any(|pair| {
        let (top, bottom) = (pair[0], pair[1]);
        bottom - top <= BOX_MAX_HEIGHT && lines[top + 1..bottom].iter().any(|l| l.contains(CURSOR))
    })
}

/// Read the agent's state off its screen.
pub fn scrape_pane(pane: &str) -> Option<PaneSignal> {
    if pane.trim().is_empty() {
        return None;
    }
    // Waiting first: a prompt is drawn over whatever came before it, so its
    // markers coexist with earlier output while the reverse is not true.
    if pane.contains(WAITING_FOOTER) || has_numbered_cursor(pane) {
        return Some(PaneSignal::Waiting);
    }
    if pane.contains(RUNNING_HINT) {
        return Some(PaneSignal::Running);
    }
    // The box last: a waiting prompt covers it and a running agent still has
    // one, so it only means idle once neither of those matched.
    if pane.contains(IDLE_HINT) || has_input_box(pane) {
        return Some(PaneSignal::Idle);
    }
    None
}

fn map_hook_state(state: &str) -> Option<State> {
    match state {
        "running" => Some(State::Running),
        "waiting" => Some(State::Waiting),
        "idle" => Some(State::Idle),
        _ => None,
    }
}

fn detail(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Decide what to report from both sources.
///
/// The screen wins wherever it says anything definite; see the module comment
/// for why. The file contributes the tool name, and is the only source for a
/// sandbox with no agent pane.
///
/// `now` is epoch seconds *on the host*, compared against a timestamp written
/// *in the sandbox*. Both come from the same kernel clock, so the comparison is
/// sound; it only ever decides whether the file is stale, and the threshold is
/// far larger than any plausible skew.
///
/// Returns `None` when neither source knows anything, which leaves the session
/// showing whatever the gateway says about the sandbox.
pub fn combine(hook: Option<&HookStatus>, pane: Option<PaneSignal>, now: u64) -> Option<Report> {
    let fresh = hook.filter(|h| now.saturating_sub(h.at) <= HOOK_STALE_SECS);
    let tool = || fresh.and_then(|h| detail(&h.detail));

    match pane {
        // A prompt is on screen. The hooks cannot see one at all.
        Some(PaneSignal::Waiting) => Some(Report {
            state: State::Waiting,
            detail: tool(),
            source: Source::Pane,
        }),
        // The input box is ready and there is no spinner, so no turn is in
        // flight -- whatever the last event to fire happened to be. No detail:
        // a finished or interrupted turn has no tool in play.
        Some(PaneSignal::Idle) => Some(Report {
            state: State::Idle,
            detail: None,
            source: Source::Pane,
        }),
        // Working. The screen does not say what on, so the file does.
        Some(PaneSignal::Running) => Some(Report {
            state: State::Running,
            detail: tool(),
            source: Source::Pane,
        }),
        // No agent pane: an unstarted session, or tmux is gone. The file is all
        // there is.
        None => {
            let h = fresh?;
            Some(Report {
                state: map_hook_state(&h.state)?,
                detail: detail(&h.detail),
                source: Source::Hook,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real captures from a live sandbox running Claude Code 2.1.143 under
    // tmux. Recorded rather than written by hand: every marker below was
    // discovered by looking at these, and a future Claude Code that renders
    // differently should break these tests loudly.
    const WAITING_EDIT: &str = include_str!("../tests/panes/waiting-edit-permission.txt");
    const WAITING_BASH: &str = include_str!("../tests/panes/waiting-bash-permission.txt");
    const RUNNING: &str = include_str!("../tests/panes/running-tool-use.txt");
    const IDLE: &str = include_str!("../tests/panes/idle-input-box.txt");
    // The same two states under 2.1.246, where the footer became a truncated
    // list of hints. The idle capture carries neither `? for shortcuts` nor
    // anything else the older markers looked for, which is the whole reason the
    // input box is now matched by shape.
    const IDLE_ROTATED: &str = include_str!("../tests/panes/idle-rotated-hint.txt");
    const RUNNING_TRUNCATED: &str = include_str!("../tests/panes/running-truncated-hint.txt");

    #[test]
    fn every_specimen_is_classified_correctly() {
        assert_eq!(scrape_pane(WAITING_EDIT), Some(PaneSignal::Waiting));
        assert_eq!(scrape_pane(WAITING_BASH), Some(PaneSignal::Waiting));
        assert_eq!(scrape_pane(RUNNING), Some(PaneSignal::Running));
        assert_eq!(scrape_pane(IDLE), Some(PaneSignal::Idle));
        assert_eq!(scrape_pane(IDLE_ROTATED), Some(PaneSignal::Idle));
        assert_eq!(scrape_pane(RUNNING_TRUNCATED), Some(PaneSignal::Running));
    }

    /// The specimen has to be the awkward case it was collected for, or the
    /// test above proves nothing about the newer version.
    #[test]
    fn the_rotated_idle_specimen_carries_none_of_the_old_markers() {
        assert!(
            !IDLE_ROTATED.contains(IDLE_HINT),
            "the tip slot held something else in this capture"
        );
        assert!(!IDLE_ROTATED.contains(RUNNING_HINT));
        assert!(!IDLE_ROTATED.contains(WAITING_FOOTER));
        assert!(has_input_box(IDLE_ROTATED), "so only the shape is left");
    }

    /// A working agent keeps its input box, so the box must never outvote the
    /// running marker. This is the ordering the whole structural check rests on.
    #[test]
    fn a_running_agent_is_not_read_as_idle_for_having_an_input_box() {
        assert!(has_input_box(RUNNING_TRUNCATED));
        assert!(has_input_box(RUNNING));
        assert_eq!(scrape_pane(RUNNING_TRUNCATED), Some(PaneSignal::Running));
        assert_eq!(scrape_pane(RUNNING), Some(PaneSignal::Running));
    }

    /// And a permission prompt outranks both: it is drawn over the box, and it
    /// is the one state the tool exists to surface.
    #[test]
    fn a_permission_prompt_outranks_the_input_box() {
        let pane = format!("{IDLE_ROTATED}\n Do you want to proceed?\n ❯ 1. Yes\n\n Esc to cancel");
        assert_eq!(scrape_pane(&pane), Some(PaneSignal::Waiting));
    }

    #[test]
    fn the_input_box_is_not_found_where_there_is_none() {
        assert!(!has_input_box(""));
        assert!(!has_input_box("sandbox@sbx:/sandbox/repo$ "));
        // A rule with no prompt between the two: the agent printing a divider.
        let dividers = format!("{r}\nsome output\n{r}", r = "─".repeat(80));
        assert!(!has_input_box(&dividers));
        // An edit confirmation draws a dashed rule, not this one, and a `❯` on a
        // numbered option. Matching it here would read every permission prompt
        // as an idle input box.
        assert!(!has_input_box(WAITING_EDIT));
        assert!(!has_input_box(WAITING_BASH));
        // A short run is something inside the agent's own output, not the box.
        let short = format!("{r}\n❯ hello\n{r}", r = "─".repeat(5));
        assert!(!has_input_box(&short));
    }

    #[test]
    fn an_empty_pane_says_nothing() {
        assert_eq!(scrape_pane(""), None);
        assert_eq!(scrape_pane("   \n\n  "), None);
        // A shell prompt in a sandbox started with --no-start.
        assert_eq!(scrape_pane("sandbox@sbx:/sandbox/repo$ "), None);
    }

    /// The trap found while collecting the specimens: the idle input box draws
    /// the same `❯` glyph as an open menu (`❯ commit this`). Keying on the
    /// glyph alone reports every idle session as needing attention, which is
    /// worse than reporting none of them.
    #[test]
    fn the_cursor_glyph_alone_does_not_mean_waiting() {
        assert!(
            IDLE.contains(CURSOR),
            "the specimen must still contain the glyph, or this proves nothing"
        );
        assert_eq!(scrape_pane(IDLE), Some(PaneSignal::Idle));

        assert!(!has_numbered_cursor("❯ commit this"));
        assert!(!has_numbered_cursor("❯ "));
        assert!(!has_numbered_cursor("❯ 3 files changed"));
        assert!(has_numbered_cursor("❯ 1. Yes"));
        assert!(has_numbered_cursor(" ❯ 12. Something"));
    }

    #[test]
    fn parses_what_the_hook_script_writes() {
        let h = parse_hook(r#"{"state":"waiting","at":1787558900,"detail":"Bash"}"#).unwrap();
        assert_eq!(h.state, "waiting");
        assert_eq!(h.at, 1787558900);
        assert_eq!(h.detail, "Bash");

        // An empty detail is normal: Stop and SessionStart carry no payload.
        let h = parse_hook(r#"{"state":"idle","at":1,"detail":""}"#).unwrap();
        assert_eq!(h.detail, "");

        assert!(parse_hook("").is_none());
        assert!(parse_hook("   ").is_none());
        assert!(parse_hook("not json").is_none());
        // A half-written file: the script renames into place to prevent this,
        // but a partial read must not panic.
        assert!(parse_hook(r#"{"state":"run"#).is_none());
    }

    fn hook(state: &str, at: u64, detail: &str) -> HookStatus {
        HookStatus {
            state: state.to_string(),
            at,
            detail: detail.to_string(),
        }
    }

    /// The first of the two findings that shaped this module. Claude Code's
    /// `Notification` hook does not fire for a permission prompt, so the file
    /// says `running` while the agent is blocked. If the file won, the one
    /// state worth being notified about would never be reported.
    #[test]
    fn the_pane_overrides_a_hook_file_that_cannot_see_the_prompt() {
        let h = hook("running", 1000, "Bash");
        let r = combine(Some(&h), Some(PaneSignal::Waiting), 1001).unwrap();
        assert_eq!(r.state, State::Waiting);
        assert_eq!(r.source, Source::Pane);
        assert_eq!(
            r.detail.as_deref(),
            Some("Bash"),
            "the tool being asked about is worth keeping"
        );
    }

    /// The second finding. Escape interrupts a turn without firing `Stop`, so
    /// the file keeps saying `running`. Before the pane was made primary this
    /// left an interrupted agent looking busy for the whole stale window.
    #[test]
    fn an_interrupted_agent_reads_as_idle_not_running() {
        let h = hook("running", 1000, "Bash");
        let r = combine(Some(&h), Some(PaneSignal::Idle), 1005).unwrap();
        assert_eq!(r.state, State::Idle, "the input box is showing");
        assert_eq!(r.source, Source::Pane);
        assert_eq!(r.detail, None, "no turn is in flight, so no tool");
    }

    #[test]
    fn a_working_agent_takes_its_tool_name_from_the_file() {
        let h = hook("running", 1000, "Edit");
        let r = combine(Some(&h), Some(PaneSignal::Running), 1030).unwrap();
        assert_eq!(r.state, State::Running);
        assert_eq!(r.detail.as_deref(), Some("Edit"));

        // A stale file contributes no detail, but the screen still decides.
        let r = combine(Some(&h), Some(PaneSignal::Running), 1001 + HOOK_STALE_SECS).unwrap();
        assert_eq!(r.state, State::Running);
        assert_eq!(r.detail, None);
    }

    /// A long-running tool fires PreToolUse and then nothing until it finishes.
    /// The screen keeps showing its interrupt hint throughout, so this stays
    /// correct however long the tool takes.
    #[test]
    fn a_long_tool_run_still_reads_as_running() {
        let h = hook("running", 1000, "Bash");
        let r = combine(
            Some(&h),
            Some(PaneSignal::Running),
            1000 + HOOK_STALE_SECS * 3,
        )
        .unwrap();
        assert_eq!(r.state, State::Running);
        assert_eq!(r.source, Source::Pane);
    }

    /// With no agent pane -- a session started with `--no-start`, or one whose
    /// tmux session died -- the file is the only source there is.
    #[test]
    fn without_a_pane_the_file_decides() {
        let h = hook("idle", 1000, "");
        let r = combine(Some(&h), None, 1030).unwrap();
        assert_eq!(r.state, State::Idle);
        assert_eq!(r.source, Source::Hook);
        assert_eq!(r.detail, None, "an empty detail is not a detail");

        let h = hook("running", 1000, "Edit");
        let r = combine(Some(&h), None, 1030).unwrap();
        assert_eq!(r.state, State::Running);
        assert_eq!(r.detail.as_deref(), Some("Edit"));

        // Stale, with nothing to fall back on: report nothing rather than a lie.
        assert!(combine(Some(&h), None, 1001 + HOOK_STALE_SECS).is_none());
    }

    #[test]
    fn clock_skew_does_not_make_a_file_stale() {
        // A sandbox clock slightly ahead of the host must not look expired.
        let h = hook("running", 2000, "Edit");
        let r = combine(Some(&h), None, 1000).unwrap();
        assert_eq!(r.state, State::Running);
    }

    /// End to end over a real specimen and the file that really accompanied it.
    #[test]
    fn the_bash_prompt_specimen_reports_waiting() {
        let h = parse_hook(r#"{"state":"running","at":1787559062,"detail":"Bash"}"#).unwrap();
        let r = combine(Some(&h), scrape_pane(WAITING_BASH), 1787559100).unwrap();
        assert_eq!(r.state, State::Waiting);
        assert_eq!(r.detail.as_deref(), Some("Bash"));
    }
}
