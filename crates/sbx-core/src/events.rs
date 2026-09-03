//! The allow/deny feed.
//!
//! `openshell logs <sandbox>` emits OCSF lines for every policy decision the
//! supervisor makes. "The agent tried to reach pastebin.com and was denied", as
//! a live event, is the thing this tool can show that claude-squad structurally
//! cannot -- so it gets a pane.
//!
//! Kept on disk per session, because the gateway's window is too small to be a
//! record; see [`merge_kept`].
//!
//! This is a gateway call rather than an exec, which matters: it does not
//! contend with the serialised per-sandbox exec budget the diff and poll panes
//! share, so the feed can refresh on its own timer without delaying anything.

use std::fs;
use std::path::{Path, PathBuf};

/// Verdict of a policy decision.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Verdict {
    Allowed,
    Denied,
    /// A lifecycle or configuration event, which decides nothing.
    Neutral,
}

/// Severity as the gateway grades it. Anything above `Info` is worth colouring:
/// the `tls: terminate` deprecation only ever appeared as a `Med`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum Severity {
    Info,
    Medium,
    High,
    Critical,
    Other,
}

impl Severity {
    fn parse(s: &str) -> Self {
        match s {
            "INFO" => Severity::Info,
            "MED" => Severity::Medium,
            "HIGH" => Severity::High,
            "CRIT" | "CRITICAL" => Severity::Critical,
            _ => Severity::Other,
        }
    }

    pub fn is_notable(self) -> bool {
        self > Severity::Info
    }
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Event {
    /// Epoch seconds. The gateway prints fractional seconds; the fraction is
    /// dropped because the feed shows a wall-clock time, not a duration.
    // `number`, not the `bigint` ts-rs assumes for a u64: serde_json writes it
    // as a JSON number and `JSON.parse` reads one back, so `bigint` would be a
    // type the runtime never produces. Epoch seconds are exact in a double
    // until the year 285000000.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub at: u64,
    /// `NET:OPEN`, `HTTP:GET`, `CONFIG:VALIDATED`.
    pub class: String,
    pub severity: Severity,
    pub verdict: Verdict,
    /// What the event is about: `curl(79) -> pastebin.com:443`.
    pub subject: String,
    /// The rule that decided, when one did. `-` in the log means none matched,
    /// and is normalised to `None`.
    pub policy: Option<String>,
    pub reason: Option<String>,
}

/// How many events to keep per session on disk.
///
/// Enough to be a record of a session's afternoon; small enough that reading it
/// back on every fetch stays free. Trimmed oldest-first.
const KEPT: usize = 4000;

/// Where a session's kept events live.
fn kept_path(session: &str) -> PathBuf {
    // Beside the session cache, under a directory of its own so a session name
    // can never collide with `sessions.json`.
    crate::store::Store::default_path()
        .with_file_name("events")
        .join(format!("{session}.jsonl"))
}

/// Add what was just fetched to what was already known, and keep the result.
///
/// The gateway's log is a rolling window and sbx is the thing making it roll:
/// every exec it takes to read a sandbox writes three lines of its own, so at the
/// intervals of increment 17 a 1500-line window covers about two minutes and held
/// *one* event worth showing. Closing the tool and opening it again therefore
/// looked like the feed had been cleared -- and for anything older than those two
/// minutes, it had been.
///
/// So the feed is now ours to keep. Each fetch is merged into a file per session,
/// deduplicated, and trimmed; the pane draws the union. Losing the file costs the
/// history and nothing else, like the session cache beside it.
pub fn merge_kept(session: &str, fetched: Vec<Event>) -> Vec<Event> {
    let path = kept_path(session);
    let mut all = read_kept(&path);
    let mut known: std::collections::HashSet<(u64, String, String)> =
        all.iter().map(identity).collect();

    let mut added = false;
    for e in fetched {
        // Inserted as they are taken, because one window can carry the same line
        // twice -- two identical execs inside the timestamp's resolution -- and a
        // set that only knows the file would keep both.
        if known.insert(identity(&e)) {
            all.push(e);
            added = true;
        }
    }
    if !added && !all.is_empty() {
        return newest_first(all);
    }

    // Oldest first while trimming, so the tail that survives is the newest.
    all.sort_by_key(|e| e.at);
    if all.len() > KEPT {
        all.drain(..all.len() - KEPT);
    }
    if let Err(e) = write_kept(&path, &all) {
        // A feed that cannot be persisted is still a feed; the pane shows what
        // was fetched and the next attempt may work.
        eprintln!("sbx: could not keep events for {session}: {e}");
    }
    newest_first(all)
}

/// What makes two events the same event. The gateway's own line, in effect: a
/// timestamp plus what it was about.
fn identity(e: &Event) -> (u64, String, String) {
    e.key()
}

fn newest_first(mut events: Vec<Event>) -> Vec<Event> {
    events.sort_by_key(|e| std::cmp::Reverse(e.at));
    events
}

fn read_kept(path: &Path) -> Vec<Event> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    // A line that will not parse is skipped rather than failing the read: this
    // is a cache, and half a history is better than none.
    text.lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn write_kept(path: &Path, events: &[Event]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let body: Vec<String> = events
        .iter()
        .map(|e| serde_json::to_string(e).expect("an Event is plain data"))
        .collect();
    // Temp file and rename, like the session cache: an interrupted write must not
    // truncate the history.
    let tmp = path.with_extension("jsonl.tmp");
    fs::write(&tmp, body.join("\n"))?;
    fs::rename(&tmp, path)
}

/// Forget a session's kept events. Called when the session is destroyed.
pub fn forget_kept(session: &str) {
    let _ = fs::remove_file(kept_path(session));
}

/// The endpoint an event was about, when it was about one.
///
/// This is what makes the feed actionable rather than only readable: a denial
/// names a host, a port and usually the binary that reached for it, which is
/// exactly the shape `policy update` takes. Everything else in the pane is
/// prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// `pastebin.com:443`. The unit `--add-endpoint` and `--remove-endpoint`
    /// both address, so the whole feature is expressed in these.
    pub endpoint: String,
    /// The kernel-resolved path the connection came from, when the decision was
    /// an L4 one. Absent for an L7 rule, which judges a method and a path and
    /// never names a binary -- and absent is load-bearing: an endpoint rule with
    /// no binaries grants nothing, so an allow with nothing to bind to is
    /// refused rather than issued.
    pub binary: Option<String>,
}

impl Event {
    /// What this event was about, as an endpoint.
    ///
    /// Three shapes come back on one feed and all three have to be read:
    ///
    /// ```text
    /// /usr/bin/curl(79) -> pastebin.com:443     an L4 decision, with a binary
    /// GET httpbin.org:443/ip                    an L7 decision, with a path
    /// host.openshell.internal:17670             a bare authority
    /// ```
    ///
    /// Anything else -- a `CONFIG:VALIDATED` warning is a whole English
    /// sentence -- is not about an endpoint, and says so rather than being
    /// coerced into one. That is why the L7 arm insists on exactly two words
    /// with an uppercase method first: a sentence ending in something that
    /// happens to parse as `host:port` must not become a policy change.
    pub fn target(&self) -> Option<Target> {
        let subject = self.subject.trim();

        // `/usr/bin/curl(79) -> pastebin.com:443`
        if let Some((left, right)) = subject.split_once(" -> ") {
            return Some(Target {
                endpoint: host_port(right)?,
                binary: binary_path(left),
            });
        }

        // `GET httpbin.org:443/ip`
        let mut words = subject.split_whitespace();
        if let (Some(method), Some(rest), None) = (words.next(), words.next(), words.next())
            && !method.is_empty()
            && method.chars().all(|c| c.is_ascii_uppercase())
        {
            let authority = rest.split('/').next().unwrap_or(rest);
            return Some(Target {
                endpoint: host_port(authority)?,
                binary: None,
            });
        }

        Some(Target {
            endpoint: host_port(subject)?,
            binary: None,
        })
    }

    /// What makes two events the same event, for anything that has to keep hold
    /// of one across a refetch.
    ///
    /// The feed grows at the top, so a row index is not a handle: three arrivals
    /// between two keystrokes and it points at something else. See the events
    /// pane's cursor.
    pub fn key(&self) -> (u64, String, String) {
        (self.at, self.class.clone(), self.subject.clone())
    }
}

/// `host:port`, or nothing.
///
/// Strict on purpose. This decides whether a line in a feed can be turned into
/// a policy change, so a loose match is a change to an endpoint nobody named.
fn host_port(s: &str) -> Option<String> {
    let (host, port) = s.trim().rsplit_once(':')?;
    // A hostname, not a sentence: the dot requirement is what keeps
    // `deprecated: 443` out, and there is no single-label host worth reaching
    // from a sandbox.
    if host.is_empty()
        || !host.contains('.')
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        return None;
    }
    let port: u16 = port.parse().ok()?;
    (port != 0).then(|| format!("{host}:{port}"))
}

/// `/usr/bin/curl(79)` -> `/usr/bin/curl`.
///
/// Absolute paths only: the policy matches on the kernel-resolved `/proc/<pid>/exe`,
/// so anything that is not one is not a path the gateway would accept.
fn binary_path(s: &str) -> Option<String> {
    let path = s.trim();
    let path = path.rsplit_once('(').map_or(path, |(p, _)| p).trim();
    path.starts_with('/').then(|| path.to_string())
}

impl Event {
    /// `HH:MM:SS` in local time, for the feed's left column.
    ///
    /// Computed by hand rather than with a date crate: this is the only place
    /// in `sbx` that formats a clock time, and the whole binary is otherwise
    /// free of a time-zone database. Uses UTC, and says so in the pane title,
    /// because guessing the offset would be worse than being explicit.
    pub fn clock_utc(&self) -> String {
        let secs = self.at % 86_400;
        format!(
            "{:02}:{:02}:{:02}",
            secs / 3600,
            (secs % 3600) / 60,
            secs % 60
        )
    }
}

/// Bracketed groups the log line puts before the payload.
const LEADING_GROUPS: usize = 4;

/// Parse the log output of `openshell logs` into policy decisions.
///
/// Non-OCSF lines are dropped. They are the supervisor's own tracing, at a
/// level of detail (`Resolved policy binary symlink via container filesystem`)
/// that belongs in a bug report rather than a feed.
pub fn parse(logs: &str) -> Vec<Event> {
    logs.lines()
        .filter_map(parse_line)
        .filter(is_worth_showing)
        .collect()
}

/// Whether an event belongs in an allow/deny feed.
///
/// Both halves of this were found by building the pane and watching a real
/// denial scroll off the top within a second.
///
/// `sbx` polls once a second and every poll opens an exec. Each exec logs an
/// ssh relay open, an `SSH:OPEN ALLOWED`, a relay close, and a pair of
/// `CONFIG:APPLYING`/`CONFIG:BUILT` lines as Landlock is applied to the new
/// process -- five events per second, every one of them the observer rather
/// than the observed.
///
/// So: keep the decisions, and keep anything the gateway graded above routine
/// -- that second clause is what preserves `CONFIG:VALIDATED [MED]`, which is
/// the only channel the gateway has for saying a policy key is deprecated, and
/// is how the `tls: terminate` removal was found. Everything else is startup
/// chatter or `sbx` looking at the sandbox.
fn is_worth_showing(e: &Event) -> bool {
    if e.class.starts_with("SSH:") || e.subject.contains("ssh relay") {
        return false;
    }
    e.verdict != Verdict::Neutral || e.severity.is_notable()
}

fn parse_line(line: &str) -> Option<Event> {
    let rest = line.trim();

    // [timestamp] [source] [level] [logger] then the payload.
    let mut groups = Vec::with_capacity(LEADING_GROUPS);
    let mut cursor = rest;
    for _ in 0..LEADING_GROUPS {
        let inner = cursor.strip_prefix('[')?;
        let (group, after) = inner.split_once(']')?;
        groups.push(group.trim());
        cursor = after.trim_start();
    }
    // Only OCSF lines are policy decisions.
    if groups[3] != "ocsf" {
        return None;
    }
    // The fraction is real precision, but the feed shows a clock time.
    let at = groups[0].split('.').next()?.parse::<u64>().ok()?;

    // CLASS:ACTIVITY [SEV] payload
    let (class, after) = cursor.split_once(' ')?;
    if !class.contains(':') {
        return None;
    }
    let after = after.trim_start();
    let (severity, payload) = match after.strip_prefix('[').and_then(|s| s.split_once(']')) {
        Some((sev, tail)) => (Severity::parse(sev.trim()), tail.trim()),
        // A shape this build has not seen. Kept rather than dropped: an
        // unparsed decision is still a decision.
        None => (Severity::Other, after),
    };

    let mut event = Event {
        at,
        class: class.to_string(),
        severity,
        verdict: Verdict::Neutral,
        subject: String::new(),
        policy: None,
        reason: None,
    };
    fill_payload(&mut event, payload);
    Some(event)
}

/// Split `ALLOWED <subject> [policy:x engine:y] [reason:z]` into its parts.
fn fill_payload(event: &mut Event, payload: &str) {
    let mut subject = String::new();
    let mut cursor = payload;

    // The verdict, when there is one, is the first word.
    for (word, verdict) in [("ALLOWED", Verdict::Allowed), ("DENIED", Verdict::Denied)] {
        if let Some(tail) = cursor.strip_prefix(word) {
            event.verdict = verdict;
            cursor = tail.trim_start();
            break;
        }
    }

    // Then free text up to the first bracketed group, and the groups after it.
    while !cursor.is_empty() {
        match cursor.find('[') {
            Some(start) => {
                subject.push_str(&cursor[..start]);
                let after = &cursor[start + 1..];
                // The gateway truncates long reasons with a trailing `...` and
                // no closing bracket, so an unterminated group is normal.
                let (group, tail) = match after.split_once(']') {
                    Some((g, t)) => (g, t),
                    None => (after, ""),
                };
                absorb_group(event, group);
                cursor = tail;
            }
            None => {
                subject.push_str(cursor);
                break;
            }
        }
    }
    // Only if there is something to set. An event whose whole description
    // lives in a `msg:` group has no free text at all, and assigning the empty
    // accumulator would throw away what `absorb_group` already recovered.
    let subject = subject.trim();
    if !subject.is_empty() {
        event.subject = strip_scheme(subject);
    }
}

/// Drop the `http://` an L7 decision carries.
///
/// The gateway logs the request it inspected, and after TLS termination that
/// request is plaintext HTTP inside the sandbox -- so `http://github.com:443`
/// is accurate about what the proxy saw and actively misleading about what left
/// the machine. Seven columns of a narrow pane spent inviting the reader to
/// wonder whether their traffic is in the clear.
fn strip_scheme(subject: &str) -> String {
    subject.replace("http://", "")
}

fn absorb_group(event: &mut Event, group: &str) {
    if let Some(reason) = group.strip_prefix("reason:") {
        event.reason = Some(reason.trim().to_string());
        return;
    }
    // `msg:` carries the whole description for events with no subject of their
    // own, so it becomes the subject rather than being thrown away.
    if let Some(msg) = group.strip_prefix("msg:") {
        if event.subject.is_empty() {
            event.subject = msg.trim().to_string();
        }
        return;
    }
    // `[policy:github_git engine:opa]` is one group holding two fields.
    for field in group.split_whitespace() {
        if let Some(p) = field.strip_prefix("policy:")
            && p != "-"
        {
            event.policy = Some(p.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured verbatim from `openshell logs` on 0.0.110, by exercising an
    /// allowed and a denied path from inside a sandbox running
    /// `policies/feature-work.yaml`.
    const LOG: &str = r#"[1787568598.279] [sandbox] [OCSF ] [ocsf] CONFIG:VALIDATED [MED] L7 policy validation warning: claude_code.endpoints[0]: 'tls: terminate' is deprecated; TLS termination is now automatic. Use 'tls: skip' to disable.
[1787568645.144] [sandbox] [OCSF ] [ocsf] NET:OPEN [INFO] [msg:ssh relay open (channel_id=8802c05b-c736-409b-8904-25d2a0231d57, target=unix:/run/openshell/ssh.sock)]
[1787568645.145] [sandbox] [OCSF ] [ocsf] SSH:OPEN [INFO] ALLOWED
[1787568645.329] [sandbox] [OCSF ] [ocsf] NET:OPEN [INFO] ALLOWED /usr/lib/git-core/git-remote-http(72) -> github.com:443 [policy:github_git engine:opa]
[1787568645.379] [sandbox] [OCSF ] [ocsf] HTTP:GET [INFO] ALLOWED GET http://github.com:443/octocat/Hello-World.git/info/refs [policy:github_git engine:l7]
[1787568645.883] [sandbox] [OCSF ] [ocsf] NET:OPEN [MED] DENIED /usr/bin/curl(79) -> pastebin.com:443 [policy:- engine:opa] [reason:endpoint pastebin.com:443 is not allowed by any policy]
[1787568980.997] [sandbox] [OCSF ] [ocsf] HTTP:GET [MED] DENIED GET http://httpbin.org:443/ip [policy:strict engine:l7] [reason:L7_REQUEST deny GET httpbin.org:443/ip reason=GET /ip not permitted by policy]
[1787568688.750] [sandbox] [INFO ] [openshell_supervisor_network::opa] Resolved policy binary symlink via container filesystem: original=/usr/lib/git-core/git-remote-https pid=56
[1787568598.674] [gateway] [INFO ] [openshell_server::grpc::policy] applied policy revision
[1787568688.958] [sandbox] [OCSF ] [ocsf] CONFIG:APPLYING [INFO] Applying Landlock filesystem sandbox [abi:V2 compat:BestEffort ro:7 rw:4]
[1787568688.958] [sandbox] [OCSF ] [ocsf] CONFIG:BUILT [INFO] Landlock ruleset built [rules_applied:10 skipped:1]
[1787568598.690] [sandbox] [OCSF ] [ocsf] PROC:LAUNCH [INFO] sleep(56)
[1787568598.705] [sandbox] [OCSF ] [ocsf] NET:OPEN [INFO] host.openshell.internal:17670
"#;

    fn ev(at: u64, subject: &str) -> Event {
        Event {
            at,
            class: "NET:OPEN".into(),
            severity: Severity::Info,
            verdict: Verdict::Allowed,
            subject: subject.into(),
            policy: None,
            reason: None,
        }
    }

    /// `XDG_CONFIG_HOME` is process-wide, so tests that point it somewhere else
    /// cannot run beside each other: one would move the kept file out from under
    /// another mid-test.
    static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A directory of its own, so the kept file can be exercised for real.
    struct Home {
        dir: PathBuf,
        previous: Option<std::ffi::OsString>,
        /// Held for the test's lifetime, not read: dropping it is the point.
        _serialised: std::sync::MutexGuard<'static, ()>,
    }

    impl Home {
        fn new(tag: &str) -> Self {
            // A poisoned lock means another test already failed; taking it anyway
            // keeps this one's failure its own rather than a second symptom.
            let guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
            let dir = std::env::temp_dir().join(format!("sbx-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            let previous = std::env::var_os("XDG_CONFIG_HOME");
            // `kept_path` is derived from the same place the session cache is, so
            // pointing that at a temporary directory is what makes this testable.
            unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };
            Home {
                dir,
                previous,
                _serialised: guard,
            }
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            match &self.previous {
                Some(v) => unsafe { std::env::set_var("XDG_CONFIG_HOME", v) },
                None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
            }
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    /// The feed has to survive the tool closing, which is what it could not do:
    /// the gateway's window is about two minutes wide at these poll intervals, so
    /// anything older than that was gone -- and reopening looked like a wipe.
    #[test]
    fn kept_events_outlive_the_window_they_came_from() {
        let _home = Home::new("events-keep");

        // A first look, which is all the gateway still had.
        let first = merge_kept("s", vec![ev(100, "curl -> a"), ev(200, "curl -> b")]);
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].at, 200, "newest first, like a feed");

        // Later, the window has rolled: only the newest is still in it, plus one
        // that has happened since.
        let second = merge_kept("s", vec![ev(200, "curl -> b"), ev(300, "curl -> c")]);
        let times: Vec<u64> = second.iter().map(|e| e.at).collect();
        assert_eq!(times, vec![300, 200, 100], "the old one is still there");

        // And a fetch that returns nothing at all -- an unreachable gateway, a
        // window with no events in it -- must not empty the feed.
        let third = merge_kept("s", vec![]);
        assert_eq!(third.len(), 3);
    }

    #[test]
    fn the_same_event_is_never_kept_twice() {
        let _home = Home::new("events-dedupe");
        merge_kept("s", vec![ev(100, "curl -> a")]);
        let again = merge_kept("s", vec![ev(100, "curl -> a"), ev(100, "curl -> a")]);
        assert_eq!(again.len(), 1);

        // And twice inside one fetch, which nothing on the file can catch.
        let once = merge_kept("t", vec![ev(100, "curl -> a"), ev(100, "curl -> a")]);
        assert_eq!(once.len(), 1, "a window carrying the same line twice");
    }

    #[test]
    fn the_kept_file_is_trimmed_and_forgotten_with_its_session() {
        let _home = Home::new("events-trim");
        let many: Vec<Event> = (0..KEPT as u64 + 50).map(|i| ev(i, "x")).collect();
        let kept = merge_kept("s", many);
        assert_eq!(kept.len(), KEPT, "trimmed to the cap");
        assert_eq!(
            kept[0].at,
            KEPT as u64 + 49,
            "and it is the newest that stay"
        );

        forget_kept("s");
        assert!(merge_kept("s", vec![]).is_empty(), "nothing is left");
    }

    #[test]
    fn keeps_only_ocsf_policy_decisions() {
        let events = parse(LOG);
        // Everything that is not a decision is dropped: the two plain-tracing
        // lines, the two ssh-relay events, the Landlock pair, the process
        // launch and the supervisor's own connection to the gateway. The
        // deprecation warning survives on severity alone.
        let classes: Vec<&str> = events.iter().map(|e| e.class.as_str()).collect();
        assert_eq!(
            classes,
            vec![
                "CONFIG:VALIDATED",
                "NET:OPEN",
                "HTTP:GET",
                "NET:OPEN",
                "HTTP:GET"
            ]
        );
    }

    /// Landlock is applied to every process the sandbox launches, which
    /// includes every exec sbx makes to poll it. Two events per poll, forever.
    #[test]
    fn the_feed_excludes_routine_landlock_chatter() {
        for line in LOG
            .lines()
            .filter(|l| l.contains("Landlock") || l.contains("PROC:LAUNCH"))
        {
            let e = parse_line(line).expect("it parses");
            assert_eq!(e.verdict, Verdict::Neutral);
            assert!(!is_worth_showing(&e), "{line} must not reach the feed");
        }
    }

    /// The feed exists to show this line. Every field of it has to survive.
    #[test]
    fn parses_a_denial_in_full() {
        let denial = parse(LOG)
            .into_iter()
            .find(|e| e.subject.contains("pastebin"))
            .expect("the denial");
        assert_eq!(denial.verdict, Verdict::Denied);
        assert_eq!(denial.class, "NET:OPEN");
        assert_eq!(denial.severity, Severity::Medium);
        assert!(denial.severity.is_notable());
        assert_eq!(denial.subject, "/usr/bin/curl(79) -> pastebin.com:443");
        assert_eq!(
            denial.reason.as_deref(),
            Some("endpoint pastebin.com:443 is not allowed by any policy")
        );
        // `policy:-` means nothing matched, which is not a rule called "-".
        assert_eq!(denial.policy, None);
    }

    #[test]
    fn parses_an_allow_and_credits_the_rule() {
        let allow = parse(LOG)
            .into_iter()
            .find(|e| e.subject.contains("git-remote-http"))
            .expect("the allow");
        assert_eq!(allow.verdict, Verdict::Allowed);
        assert_eq!(allow.policy.as_deref(), Some("github_git"));
        assert_eq!(
            allow.subject,
            "/usr/lib/git-core/git-remote-http(72) -> github.com:443"
        );
        assert!(allow.reason.is_none());
        assert!(!allow.subject.contains("http://"));
        assert!(!allow.severity.is_notable());
    }

    /// An L7 decision names a method and a path, not a binary. Both shapes come
    /// back on the same feed and both have to read correctly.
    #[test]
    fn parses_an_l7_path_denial() {
        let e = parse(LOG)
            .into_iter()
            .find(|e| e.subject.contains("httpbin"))
            .expect("the l7 denial");
        assert_eq!(e.verdict, Verdict::Denied);
        assert_eq!(e.class, "HTTP:GET");
        // No scheme: the request the proxy inspected was plaintext, but the
        // connection out of the sandbox was not, and `http://` reads as a
        // claim about the latter.
        assert_eq!(e.subject, "GET httpbin.org:443/ip");
        assert_eq!(e.policy.as_deref(), Some("strict"));
        assert!(
            e.reason
                .as_deref()
                .unwrap()
                .contains("not permitted by policy")
        );
    }

    /// A configuration warning decides nothing but is the only channel the
    /// gateway has for telling you a policy key is deprecated -- which is how
    /// the `tls: terminate` removal was found in the first place.
    #[test]
    fn a_config_warning_is_neutral_but_notable() {
        let e = &parse(LOG)[0];
        assert_eq!(e.verdict, Verdict::Neutral);
        assert_eq!(e.severity, Severity::Medium);
        assert!(e.subject.contains("'tls: terminate' is deprecated"));
        assert!(e.policy.is_none());
    }

    /// sbx polls once a second and every poll opens an exec, which logs three
    /// events of its own. Without the filter the feed shows nothing else.
    #[test]
    fn the_feed_excludes_sbx_watching_the_sandbox() {
        for line in LOG
            .lines()
            .filter(|l| l.contains("ssh relay") || l.contains("SSH:OPEN"))
        {
            let e = parse_line(line).expect("it parses");
            assert!(!is_worth_showing(&e), "{line} must not reach the feed");
        }
    }

    #[test]
    fn a_truncated_reason_still_parses() {
        // The gateway cuts long reasons off mid-word, leaving the group
        // unterminated. Requiring a closing bracket would drop the event.
        let line = "[1787568645.883] [sandbox] [OCSF ] [ocsf] NET:OPEN [MED] DENIED /usr/bin/pip(9) -> pypi.org:443 [policy:- engine:opa] [reason:binary not allowed. SYMLINK ...";
        let e = parse_line(line).expect("an event");
        assert_eq!(e.verdict, Verdict::Denied);
        assert_eq!(e.subject, "/usr/bin/pip(9) -> pypi.org:443");
        assert!(
            e.reason
                .as_deref()
                .unwrap()
                .starts_with("binary not allowed")
        );
    }

    #[test]
    fn malformed_lines_are_dropped_rather_than_panicking() {
        for line in [
            "",
            "   ",
            "not a log line at all",
            "[1787568645.883] [sandbox]",
            "[nan] [sandbox] [OCSF ] [ocsf] NET:OPEN [MED] DENIED x",
            "[1787568645.883] [sandbox] [OCSF ] [ocsf] noclass payload",
            "[1787568645.883] [sandbox] [OCSF ] [ocsf]",
        ] {
            assert!(parse_line(line).is_none(), "{line:?} must not parse");
        }
        assert!(parse("").is_empty());
    }

    #[test]
    fn formats_a_clock_time() {
        // 1787568645 is 10:50:45 UTC; only the time of day is shown.
        let e = &parse(LOG)[1];
        assert_eq!(e.at, 1_787_568_645);
        assert_eq!(e.clock_utc(), "10:50:45");
        // Midnight must render as 00:00:00 rather than 24:00:00.
        let midnight = Event {
            at: 1_787_529_600,
            ..e.clone()
        };
        assert_eq!(midnight.clock_utc(), "00:00:00");
    }

    /// The feed is only actionable if a line can be turned back into the
    /// endpoint it was about. Both decision shapes have to yield one, and the
    /// L4 shape has to yield the binary too -- an endpoint rule with no
    /// binaries grants nothing.
    #[test]
    fn a_decision_yields_the_endpoint_it_was_about() {
        let events = parse(LOG);

        let denial = events
            .iter()
            .find(|e| e.subject.contains("pastebin"))
            .unwrap();
        assert_eq!(
            denial.target(),
            Some(Target {
                endpoint: "pastebin.com:443".into(),
                binary: Some("/usr/bin/curl".into()),
            })
        );

        let allow = events
            .iter()
            .find(|e| e.subject.contains("git-remote-http"))
            .unwrap();
        assert_eq!(
            allow.target(),
            Some(Target {
                endpoint: "github.com:443".into(),
                binary: Some("/usr/lib/git-core/git-remote-http".into()),
            })
        );

        // An L7 decision judges a method and a path, so it names no binary --
        // which is the case the pane has to refuse an allow for.
        let l7 = events
            .iter()
            .find(|e| e.subject.contains("httpbin"))
            .unwrap();
        assert_eq!(
            l7.target(),
            Some(Target {
                endpoint: "httpbin.org:443".into(),
                binary: None,
            })
        );
    }

    /// A bare authority is how the supervisor's own connections are logged, and
    /// it is a perfectly good endpoint.
    #[test]
    fn a_bare_authority_is_an_endpoint() {
        let e = ev(1, "host.openshell.internal:17670");
        assert_eq!(
            e.target(),
            Some(Target {
                endpoint: "host.openshell.internal:17670".into(),
                binary: None,
            })
        );
    }

    /// The whole risk of this parse: a subject that is prose must not become a
    /// policy change. `CONFIG:VALIDATED` carries an English sentence with
    /// colons in it, and there is exactly one keystroke between a match here
    /// and a rule at the gateway.
    #[test]
    fn prose_is_never_mistaken_for_an_endpoint() {
        let warning = &parse(LOG)[0];
        assert!(warning.subject.contains("deprecated"));
        assert_eq!(warning.target(), None, "{}", warning.subject);

        for subject in [
            "",
            "sleep(56)",
            "applied policy revision",
            // A single-label host: nothing worth reaching from a sandbox, and
            // allowing it would be allowing a word.
            "localhost:443",
            // A port that is not one.
            "pastebin.com:https",
            "pastebin.com:0",
            "pastebin.com:99999",
            // A lowercase first word is not an HTTP method, so this is prose.
            "get httpbin.org:443/ip",
            // Three words is prose too, whatever the last one looks like.
            "denied reaching pastebin.com:443",
        ] {
            assert_eq!(ev(1, subject).target(), None, "{subject:?}");
        }
    }

    /// A relative path is not what the gateway matches on -- the policy is
    /// checked against the kernel-resolved `/proc/<pid>/exe` -- so a subject
    /// carrying one yields the endpoint without a binary rather than a binary
    /// the gateway would reject.
    #[test]
    fn only_an_absolute_path_counts_as_a_binary() {
        let e = ev(1, "curl(79) -> pastebin.com:443");
        let t = e.target().unwrap();
        assert_eq!(t.endpoint, "pastebin.com:443");
        assert_eq!(t.binary, None);
    }

    /// The pane holds on to a selected event across a refetch, and the feed
    /// grows at the top, so the handle cannot be a row index.
    #[test]
    fn the_key_identifies_an_event_across_a_refetch() {
        let a = ev(100, "curl -> a");
        let b = ev(100, "curl -> b");
        assert_ne!(a.key(), b.key(), "same second, different subject");
        assert_eq!(a.key(), a.clone().key());
        // And it is the same notion of sameness the kept file dedupes on, or
        // the cursor would follow an event the merge had discarded.
        assert_eq!(a.key(), identity(&a));
    }

    #[test]
    fn severity_orders_so_notable_means_above_info() {
        assert!(Severity::parse("MED").is_notable());
        assert!(Severity::parse("HIGH").is_notable());
        assert!(Severity::parse("CRIT").is_notable());
        assert!(!Severity::parse("INFO").is_notable());
        // An unknown grade must not be silently downgraded to routine.
        assert!(Severity::parse("WHATEVER").is_notable());
    }
}
