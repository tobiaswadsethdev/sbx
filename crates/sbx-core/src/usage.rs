//! What a session has spent, and how much of the account's allowance is gone.
//!
//! **The status line is the only place Claude Code hands this out.** There is no
//! file it keeps the numbers in and no endpoint to ask; what there is, is a
//! `statusLine` command that Claude Code invokes on every render with a JSON
//! payload on stdin -- and that payload carries the session's cost and, since
//! the release that added it, the account's rate-limit windows. So the image
//! bakes in a status line command whose real job is to keep a copy of the
//! payload where a poll can read it: `images/sbx-base/sbx-usage`, writing
//! `/sandbox/.sbx/usage.json`, exactly as `sbx-status` does for the hooks.
//!
//! **The whole payload is kept and this parses what it recognises.** The shape
//! belongs to Claude Code and grows: `rate_limits` did not exist before 2.1.x,
//! `spend_limit` arrived after that, and a reader that insisted on a shape would
//! break on an upgrade rather than showing less. Every field here is optional
//! and an unrecognised one is ignored.
//!
//! **A rate-limit window is the account's, not the session's**, which is why
//! [`Usage::windows`] is separated from the cost: two sessions on one account
//! report the same percentages, and showing them per session as though they
//! belonged to it would be a lie about what is being measured. What the
//! interface does with that -- the newest reading in the window's header, the
//! cost on the session's own pane -- is the front end's business.

use serde::{Deserialize, Serialize};

/// One rate-limit window, as the status line reports it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Window {
    /// `5h`, `7d`, `spend` -- shortened from the payload's key, because this is
    /// a label in a strip two centimetres wide.
    pub label: String,
    pub used_percentage: f64,
    /// When the window rolls over, in epoch seconds.
    ///
    /// **Measured, not assumed.** Claude Code's changelog describes
    /// `resets_at` and the obvious reading is an ISO instant; what 2.1.251
    /// actually sends is `1788434400`. A reader that asked for a string got
    /// `None` and the window showed a percentage with no reset time -- which
    /// looks like a tracker that has none rather than a parser that missed one.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub resets_at: Option<u64>,
}

/// What one session has spent, and what the account has left.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// The model the agent is actually on, as it names itself. Worth having
    /// beside the cost: the image picks one in `settings.json` and a session
    /// can be switched off it from inside.
    pub model: Option<String>,
    /// Claude Code's own version, which is the image's and not the host's.
    pub version: Option<String>,
    pub cost_usd: Option<f64>,
    /// Wall-clock milliseconds this session has been working.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub duration_ms: Option<u64>,
    pub lines_added: Option<u32>,
    pub lines_removed: Option<u32>,
    /// The account's rate-limit windows. Empty until the agent has actually
    /// called the API: they come from its answers, so a session sitting at a
    /// prompt has none -- measured, on a session that had not logged in.
    pub windows: Vec<Window>,
    /// How full the context is, as a percentage of the window.
    ///
    /// Not in the plan and worth having: it is the number that says whether a
    /// session is about to compact, which is the thing a person watching four
    /// agents actually wants to know. It is in the payload beside the cost.
    pub context_used_percentage: Option<f64>,
    /// How big that window is, so a percentage has something to be a
    /// percentage of.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub context_size: Option<u64>,
}

impl Usage {
    /// Whether there is anything worth showing.
    ///
    /// A payload arrives from the first render, before a turn has cost
    /// anything, so "the file exists" is not the same as "there is something to
    /// say".
    pub fn is_empty(&self) -> bool {
        self.cost_usd.unwrap_or(0.0) == 0.0
            && self.windows.is_empty()
            && self.context_used_percentage.unwrap_or(0.0) == 0.0
    }
}

/// Read one status line payload.
///
/// `None` for anything that is not JSON at all -- a truncated write, a script
/// that printed an error -- because a poll should show nothing rather than
/// something wrong. Pure, so every shape Claude Code has sent can be asserted
/// on without an agent.
pub fn parse(text: &str) -> Option<Usage> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(text).ok()?;

    let cost = v.get("cost");
    let mut usage = Usage {
        model: v
            .get("model")
            .and_then(|m| {
                m.get("display_name")
                    .or_else(|| m.get("id"))
                    .and_then(|s| s.as_str())
            })
            .map(str::to_string),
        version: v
            .get("version")
            .and_then(|s| s.as_str())
            .map(str::to_string),
        cost_usd: cost.and_then(|c| c.get("total_cost_usd")).and_then(number),
        duration_ms: cost
            .and_then(|c| c.get("total_duration_ms"))
            .and_then(|n| n.as_u64()),
        lines_added: cost
            .and_then(|c| c.get("total_lines_added"))
            .and_then(as_u32),
        lines_removed: cost
            .and_then(|c| c.get("total_lines_removed"))
            .and_then(as_u32),
        windows: Vec::new(),
        context_used_percentage: v
            .get("context_window")
            .and_then(|c| c.get("used_percentage"))
            .and_then(number),
        context_size: v
            .get("context_window")
            .and_then(|c| c.get("context_window_size"))
            .and_then(|n| n.as_u64()),
    };

    // The three windows that exist, in the order they are useful: the one that
    // stops you soonest first. A key that is not here yet -- and there will be
    // one -- is ignored rather than guessed at.
    if let Some(limits) = v.get("rate_limits") {
        for (key, label) in [
            ("five_hour", "5h"),
            ("seven_day", "7d"),
            ("spend_limit", "spend"),
        ] {
            if let Some(window) = limits.get(key)
                && let Some(used) = window.get("used_percentage").and_then(number)
            {
                usage.windows.push(Window {
                    label: label.to_string(),
                    used_percentage: used,
                    // Epoch seconds; a string is ignored rather than guessed
                    // at, since parsing an instant without a date library is
                    // how a wrong time gets displayed confidently.
                    resets_at: window.get("resets_at").and_then(|n| n.as_u64()),
                });
            }
        }
    }
    Some(usage)
}

/// A JSON number as a float, whichever way it was written.
///
/// `total_cost_usd` is a float and `used_percentage` has arrived as both an
/// integer and a float; `as_f64` alone reads an integer fine, but a reader that
/// asked for `as_f64` on a *string* would silently drop a value some future
/// version writes as one.
fn number(v: &serde_json::Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str()?.trim().parse().ok())
}

fn as_u32(v: &serde_json::Value) -> Option<u32> {
    v.as_u64().and_then(|n| u32::try_from(n).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Captured from a real session**, not written from the documentation:
    /// `claude` 2.1.251 in a sandbox, after one turn. Trimmed of the ids and
    /// the paths, and of the objects nothing here reads.
    ///
    /// The one that mattered is `resets_at`: it is an epoch integer, where the
    /// obvious reading of the changelog is an ISO instant.
    const PAYLOAD: &str = r#"{
      "effort": { "level": "high" },
      "session_name": "Reply with hello",
      "model": { "id": "claude-opus-5[1m]", "display_name": "Opus 5 (1M context)" },
      "workspace": { "current_dir": "/sandbox/repo", "project_dir": "/sandbox/repo" },
      "version": "2.1.251",
      "output_style": { "name": "default" },
      "cost": {
        "total_cost_usd": 0.074367,
        "total_duration_ms": 3637,
        "total_api_duration_ms": 3081,
        "total_lines_added": 0,
        "total_lines_removed": 0
      },
      "context_window": {
        "total_input_tokens": 18409,
        "total_output_tokens": 4,
        "context_window_size": 1000000,
        "current_usage": {
          "input_tokens": 2,
          "output_tokens": 4,
          "cache_creation_input_tokens": 6747,
          "cache_read_input_tokens": 11660
        },
        "used_percentage": 2,
        "remaining_percentage": 98
      },
      "exceeds_200k_tokens": false,
      "rate_limits": {
        "five_hour": { "used_percentage": 32, "resets_at": 1788434400 },
        "seven_day": { "used_percentage": 7.000000000000001, "resets_at": 1788739200 }
      }
    }"#;

    #[test]
    fn a_status_line_payload_becomes_a_usage() {
        let u = parse(PAYLOAD).expect("parsed");
        assert_eq!(u.model.as_deref(), Some("Opus 5 (1M context)"));
        assert_eq!(u.version.as_deref(), Some("2.1.251"));
        assert_eq!(u.cost_usd, Some(0.074367));
        assert_eq!(u.duration_ms, Some(3637));
        assert_eq!(u.lines_added, Some(0));
        assert!(!u.is_empty());

        // How full the context is, which is what says whether a session is
        // about to compact.
        assert_eq!(u.context_used_percentage, Some(2.0));
        assert_eq!(u.context_size, Some(1_000_000));

        // The window that stops you soonest, first, and its reset as epoch
        // seconds -- which is what it actually is.
        assert_eq!(u.windows.len(), 2);
        assert_eq!(u.windows[0].label, "5h");
        assert_eq!(u.windows[0].used_percentage, 32.0);
        assert_eq!(u.windows[0].resets_at, Some(1_788_434_400));
        assert_eq!(u.windows[1].label, "7d");
        assert_eq!(u.windows[1].resets_at, Some(1_788_739_200));
    }

    /// **The shape belongs to Claude Code and it grows.** A reader that
    /// insisted on the whole of it would show nothing after an upgrade rather
    /// than showing less, which is the wrong way round for a display.
    #[test]
    fn a_payload_missing_everything_optional_still_reads() {
        // An older Claude Code: cost, no rate limits.
        let older = parse(r#"{"model":{"id":"claude-opus-5"},"cost":{"total_cost_usd":0.1}}"#)
            .expect("parsed");
        assert_eq!(
            older.model.as_deref(),
            Some("claude-opus-5"),
            "id is the fallback"
        );
        assert!(older.windows.is_empty());
        assert_eq!(older.cost_usd, Some(0.1));

        // A newer one, with a window this version has never heard of beside two
        // it has.
        let newer = parse(
            r#"{"rate_limits":{
                 "five_hour":{"used_percentage":50},
                 "lunar_month":{"used_percentage":3},
                 "spend_limit":{"used_percentage":80,"resets_at":1793491200}
               }}"#,
        )
        .expect("parsed");
        assert_eq!(
            newer
                .windows
                .iter()
                .map(|w| w.label.as_str())
                .collect::<Vec<_>>(),
            ["5h", "spend"],
            "an unrecognised window is ignored, not guessed at"
        );
        assert_eq!(newer.cost_usd, None);
        assert!(!newer.is_empty(), "a window alone is worth showing");

        // The first render of a session that has done nothing.
        let fresh = parse(r#"{"model":{"display_name":"Opus 5"},"cost":{"total_cost_usd":0}}"#)
            .expect("parsed");
        assert!(fresh.is_empty(), "nothing spent and no windows");
    }

    /// A half-written file, or a script that printed a complaint. Showing
    /// nothing beats showing something wrong.
    #[test]
    fn anything_that_is_not_json_is_nothing() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("   \n "), None);
        assert_eq!(parse("{\"cost\":{\"total_cost"), None);
        assert_eq!(parse("jq: error: syntax error"), None);
    }
}
