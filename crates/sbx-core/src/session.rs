//! Session identity and the metadata record.
//!
//! A session is a task plus the sandbox running it. The **sandbox is the
//! source of truth**: seeding writes `/sandbox/.sbx/meta.json` inside it, so a
//! session survives losing the local cache entirely. Labels carry only
//! identity, because the gateway restricts label values to Kubernetes rules -
//! at most 63 characters of `[A-Za-z0-9._-]`, which cannot hold a repo URL or
//! a branch name containing `/`.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Marks a sandbox as ours, so discovery never touches sandboxes created by
/// hand or by another tool.
pub const LABEL_MANAGED: &str = "sbx.managed";
pub const LABEL_SESSION: &str = "sbx.session";
pub const SELECTOR_MANAGED: &str = "sbx.managed=true";

/// Where the metadata record lives inside the sandbox.
pub const META_PATH: &str = "/sandbox/.sbx/meta.json";
/// Where the repository is cloned inside the sandbox.
pub const REPO_PATH: &str = "/sandbox/repo";
/// The task prompt, written as a plain file so the shell can read it without
/// any nested quoting.
pub const TASK_PATH: &str = "/sandbox/.sbx/task.txt";
/// Where the agent's hooks record what it is doing. Written by `sbx-status`,
/// which the image bakes in; see `images/sbx-base/sbx-status`.
pub const STATUS_PATH: &str = "/sandbox/.sbx/status.json";
/// Where the seeder reports how far it has got. Written inside the sandbox by a
/// process that outlives the command that started it, which is what lets a clone
/// survive the tool quitting; see [`crate::seed`].
pub const SEED_STATE_PATH: &str = "/sandbox/.sbx/seed.state";
/// The seeder's own output, kept for when it fails.
pub const SEED_LOG_PATH: &str = "/sandbox/.sbx/seed.log";
/// Where the seeder script is written before being run detached.
pub const SEED_SCRIPT_PATH: &str = "/sandbox/.sbx/seed.sh";
/// Name of the tmux session **inside** the sandbox that the agent runs in.
///
/// tmux runs in the sandbox rather than on the host so the agent survives
/// losing its connection, and so its output can be scraped with capture-pane
/// without depending on anything host-side.
pub const TMUX_SESSION: &str = "agent";
/// Container image sbx runs sandboxes from: the community base plus tmux.
pub const IMAGE: &str = "sbx-base:latest";
/// The repository half of [`IMAGE`], which the toolchain variants share.
///
/// Its own constant because a variant tag is built from it -- `sbx-base:dotnet`
/// beside `sbx-base:latest` -- and two `format!`s spelling the name out would be
/// two places to rename it. See [`crate::toolchain::tag`].
pub const IMAGE_REPO: &str = "sbx-base";

/// Gateway limit on a label value.
const MAX_LABEL_VALUE: usize = 63;
/// Gateway limit on a sandbox name, measured against 0.0.110.
const MAX_SANDBOX_NAME: usize = 19;
/// Prefix applied to sandbox and tmux names, so ours are recognisable in
/// `openshell sandbox list` alongside sandboxes created by hand.
const PREFIX: &str = "sbx-";
/// How much of a *sandbox* name is left for the session's own name.
const MAX_NAME_IN_SANDBOX: usize = MAX_SANDBOX_NAME - PREFIX.len();

/// How long a session name may be.
///
/// Deliberately longer than a sandbox name can hold. `sbx-` plus fifteen
/// characters is what the gateway allows, and fifteen characters of a task
/// produce names like `i-want-to-add`: the *filler* survives and the subject is
/// cut off. So the session name is ours and the sandbox name is derived from it
/// -- see [`sandbox_name`] -- and the full name travels in the `sbx.session`
/// label, which has 63 characters to spend.
///
/// Forty rather than sixty-three: the name is also a branch (`sbx/<name>`), a
/// column in the list, and something you type after `sbx attach`.
const MAX_NAME: usize = 40;

/// Characters of the name kept when a sandbox name has to be shortened. The
/// rest of the budget goes to the discriminator below.
const SANDBOX_STEM: usize = 10;

/// The size to leave the agent's tmux window at when nothing is attached.
///
/// The status scraper reads that window, and Claude Code's footer -- where the
/// running marker lives -- is truncated to the pane, so the width decides
/// whether the state column can tell working from idle. Matches the image's
/// `default-size`; `image.rs` has a test that it does.
pub const SCRAPE_SIZE: (u16, u16) = (200, 50);

/// The sandbox a session of this name owns.
///
/// The convention, in one place. Deleting and adopting both have to name a
/// sandbox without a record to read it from -- that is the whole point of
/// having a convention -- and two copies of this `format!` would be two things
/// to keep in step with [`Session::new`].
pub fn sandbox_name(name: &str) -> String {
    if name.len() <= MAX_NAME_IN_SANDBOX {
        return format!("{PREFIX}{name}");
    }
    // Truncation alone would collide: `maxgaming-scala-customer-id` and
    // `maxgaming-scala-tax` share their first fifteen characters, and the two
    // sessions would name one sandbox. So a long name keeps its first ten
    // characters -- enough to recognise in `openshell sandbox list` -- and ends
    // in four hex digits of the *whole* name, which keeps this a pure function
    // of the session name. That is what makes it a convention rather than a
    // lookup: deleting or adopting a sandbox has to name it without a record to
    // read it from.
    let stem: String = name.chars().take(SANDBOX_STEM).collect();
    format!("{PREFIX}{}-{:04x}", stem.trim_end_matches('-'), tag(name))
}

/// FNV-1a, folded to sixteen bits. A hash, not a checksum: it only has to
/// scatter names that share a prefix, and writing four lines beats a dependency.
fn tag(name: &str) -> u16 {
    let mut h: u32 = 0x811c_9dc5;
    for b in name.as_bytes() {
        h ^= u32::from(*b);
        h = h.wrapping_mul(0x0100_0193);
    }
    ((h >> 16) ^ h) as u16
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum NameError {
    #[error("name is empty")]
    Empty,
    #[error(
        "name is longer than {MAX_NAME} characters (the gateway caps sandbox names at {MAX_SANDBOX_NAME})"
    )]
    TooLong,
    #[error("name must be lowercase letters, digits and dashes; `{0}` is not allowed")]
    BadChar(char),
    #[error("name must start and end with a letter or digit")]
    BadEdge,
}

/// Turn arbitrary text into a session name: lowercase, dashes, trimmed.
///
/// Used to derive a name from a task description when the user does not supply
/// one. Returns `None` if nothing usable survives.
/// Words a task wraps its subject in, dropped when deriving a name.
///
/// A task is written as a sentence -- "I want to add the MaxGaming Scala
/// customer id" -- and the first words of a sentence are almost never what it
/// is about. Keeping them spent the whole budget on `i-want-to-add`, which
/// names nothing. Verbs are *not* here: `add`, `fix`, `remove` and `update`
/// distinguish two tasks about the same subject.
const FILLER: &[&str] = &[
    "a", "also", "an", "and", "any", "are", "at", "be", "can", "could", "do", "does", "for",
    "from", "i", "in", "into", "is", "it", "its", "just", "let", "lets", "me", "my", "need",
    "needs", "of", "on", "or", "our", "please", "shall", "should", "some", "that", "the", "there",
    "these", "this", "to", "us", "want", "we", "will", "would", "you", "your",
];

pub fn slugify(text: &str) -> Option<String> {
    // Twice: once keeping only the words that carry meaning, and -- if that
    // leaves nothing, as "can you do it for me" would -- once taking the text as
    // written. A name is better than no name.
    slug(text, true).or_else(|| slug(text, false))
}

fn slug(text: &str, drop_filler: bool) -> Option<String> {
    // Split on anything that is not alphanumeric, then re-join with dashes,
    // dropping whole words once the budget is spent. Truncating mid-word would
    // turn "readme" into "read" and read as a different task.
    let mut out = String::new();
    for word in text.split(|c: char| !c.is_ascii_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        let word = word.to_ascii_lowercase();
        if drop_filler && FILLER.contains(&word.as_str()) {
            continue;
        }
        let extra = if out.is_empty() {
            word.len()
        } else {
            word.len() + 1
        };
        if out.len() + extra > MAX_NAME {
            // A single first word longer than the budget still has to yield
            // something, so hard-truncate only in that case.
            if out.is_empty() {
                out = word.chars().take(MAX_NAME).collect();
            }
            break;
        }
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(&word);
    }
    let trimmed = out.trim_matches('-').to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// A session name from the task, falling back to the repository's last path
/// segment.
///
/// Shared by `sbx new` and the TUI's create form, so the name a session gets is
/// the same however it was started -- and so the form can show the name it is
/// about to use while the task is still being typed.
pub fn derive_name(task: &str, repo: &str) -> Option<String> {
    slugify(task).or_else(|| {
        repo.trim_end_matches('/')
            .rsplit('/')
            .next()
            .map(|s| s.trim_end_matches(".git"))
            .and_then(slugify)
    })
}

/// A derived name that is not already taken.
///
/// Two sessions in the same repository is the normal case -- try something, try
/// something else -- and with no task typed yet both derive the repository's own
/// name. Refusing the second one until the name is edited by hand makes the
/// common case the one that needs work, so a counter is appended instead:
/// `inet-server`, `inet-server-2`, `inet-server-3`.
///
/// The base is shortened to make room for the suffix rather than the suffix being
/// dropped, because the gateway's name budget is the hard part and a name that no
/// longer fits it would be refused three steps later. Unlike [`slugify`], which
/// drops whole words, this cuts mid-word if it has to: `fix-the-readm-2` still
/// reads as a variant of the same thing, where `fix-the-2` would not.
pub fn unique_name(base: &str, taken: &[String]) -> String {
    let is_free = |candidate: &str| !taken.iter().any(|t| t == candidate);
    if is_free(base) {
        return base.to_string();
    }
    for n in 2..=99u32 {
        let suffix = format!("-{n}");
        let room = MAX_NAME.saturating_sub(suffix.len());
        let stem: String = base.chars().take(room).collect();
        // Trimming can leave a trailing dash, which `validate_name` rejects.
        let candidate = format!("{}{suffix}", stem.trim_end_matches('-'));
        if is_free(&candidate) {
            return candidate;
        }
    }
    // A hundred sessions of one name is not a case worth handling; hand back the
    // base and let the collision be reported as it was before.
    base.to_string()
}

/// The provider profile type carrying an agent's credential.
///
/// Used to preselect a provider in the create form: a session started without
/// the agent's credential comes up to a login prompt, which is a poor way to
/// find out. `None` for an agent whose credentials sbx knows nothing about.
pub fn agent_provider_type(agent: &str) -> Option<&'static str> {
    match agent {
        "claude" => Some("claude-code-oauth"),
        _ => None,
    }
}

/// Validate a session name against both our rules and the gateway's.
pub fn validate_name(name: &str) -> Result<(), NameError> {
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if name.len() > MAX_NAME {
        return Err(NameError::TooLong);
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-'))
    {
        return Err(NameError::BadChar(bad));
    }
    let edges_ok = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    if !name.starts_with(edges_ok) || !name.ends_with(edges_ok) {
        return Err(NameError::BadEdge);
    }
    debug_assert!(name.len() <= MAX_LABEL_VALUE);
    Ok(())
}

/// Lifecycle state.
///
/// The agent-derived states (`Running`, `Waiting`, `Idle`) are not set yet;
/// status detection arrives in a later increment. Until then a healthy session
/// sits in `Ready`.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Creating,
    Seeding,
    Ready,
    Running,
    Waiting,
    Idle,
    Failed,
    Published,
    /// The sandbox backing this session is gone.
    Dead,
}

impl std::fmt::Display for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            State::Creating => "creating",
            State::Seeding => "seeding",
            State::Ready => "ready",
            State::Running => "running",
            State::Waiting => "waiting",
            State::Idle => "idle",
            State::Failed => "failed",
            State::Published => "published",
            State::Dead => "dead",
        };
        // `pad`, not `write_str`: a Display impl that writes directly ignores
        // the formatter's width, so `{:<9}` silently does nothing and the list
        // columns run into each other.
        f.pad(s)
    }
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub name: String,
    pub sandbox: String,
    /// Name of the tmux session inside the sandbox. Stored rather than assumed
    /// so an older session keeps working if the default ever changes.
    pub tmux: String,
    pub repo: String,
    /// Branch cloned from; `None` means the remote's default.
    #[serde(default)]
    pub base_branch: Option<String>,
    pub work_branch: String,
    #[serde(default)]
    pub task: String,
    #[serde(default)]
    pub policy: Option<String>,
    #[serde(default)]
    pub providers: Vec<String>,
    /// Skills copied into this session when it was created, and where each came
    /// from on the host. A copy, not a link -- see [`crate::skills`] -- so this
    /// says what the agent has, whatever the host's copy says now.
    #[serde(default)]
    pub skills: Vec<crate::skills::Skill>,
    /// MCP servers this session's agent was given, as the config file named
    /// them when it was created.
    ///
    /// Recorded rather than re-read, for the reason the whole record exists: the
    /// sandbox is the source of truth about itself. The config file may have
    /// changed since, and what matters for reading -- or re-seeding -- this
    /// session is what it was actually created with.
    #[serde(default)]
    pub mcp: Vec<crate::mcp::Server>,
    /// Toolchains this session's image carries, by name.
    ///
    /// Recorded for the reason the whole record exists: the sandbox is the source
    /// of truth about itself. The image variant it was created from may have been
    /// rebuilt or deleted since, and what matters for reading this session is
    /// what it was actually given -- which is also what says why its policy holds
    /// a registry endpoint no template grants.
    #[serde(default)]
    pub toolchains: Vec<String>,
    #[serde(default = "default_agent")]
    pub agent: String,
    /// Epoch seconds. Deliberately not a formatted timestamp: the display wants
    /// a relative age, and storing epoch avoids a date-library dependency.
    // `number`, not the `bigint` ts-rs assumes for a u64: serde_json writes it
    // as a JSON number and `JSON.parse` reads one back, so `bigint` would be a
    // type the runtime never produces. Epoch seconds are exact in a double
    // until the year 285000000.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub created_at: u64,
    pub state: State,
}

fn default_agent() -> String {
    "claude".to_string()
}

pub fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Render an age like `3m`, `2h`, `4d` for the list view.
pub fn humanize_age(created_at: u64, now: u64) -> String {
    let secs = now.saturating_sub(created_at);
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}

impl Session {
    pub fn new(name: String, repo: String, task: String) -> Self {
        Session {
            sandbox: sandbox_name(&name),
            tmux: TMUX_SESSION.to_string(),
            work_branch: format!("sbx/{name}"),
            name,
            repo,
            base_branch: None,
            task,
            policy: None,
            providers: Vec::new(),
            skills: Vec::new(),
            mcp: Vec::new(),
            toolchains: Vec::new(),
            agent: default_agent(),
            created_at: now_epoch(),
            state: State::Creating,
        }
    }

    pub fn labels(&self) -> BTreeMap<String, String> {
        let mut l = BTreeMap::new();
        l.insert(LABEL_MANAGED.to_string(), "true".to_string());
        l.insert(LABEL_SESSION.to_string(), self.name.clone());
        l
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A variant tag is `IMAGE_REPO` plus the toolchains, and the base image is
    /// the same repository with `latest`. If those two drifted apart, every
    /// variant would be built under a name nothing looks for.
    #[test]
    fn the_base_image_and_the_variants_share_a_repository() {
        assert_eq!(IMAGE, format!("{IMAGE_REPO}:latest"));
    }

    /// Two sessions in one repository is the normal case, and with no task typed
    /// both derive the repository's name. The second must not need hand-editing.
    #[test]
    fn a_taken_name_gets_a_counter() {
        let taken = vec!["inet-server".to_string()];
        assert_eq!(unique_name("inet-server", &taken), "inet-server-2");
        assert_eq!(
            unique_name("other", &taken),
            "other",
            "free names are left be"
        );

        let taken = vec!["inet-server".into(), "inet-server-2".into()];
        assert_eq!(unique_name("inet-server", &taken), "inet-server-3");
    }

    /// The suffix has to fit inside the gateway's budget, or the name it produces
    /// is refused three steps later.
    #[test]
    fn the_counter_fits_the_name_limit() {
        let base = "a".repeat(MAX_NAME);
        let taken = vec![base.clone()];
        let next = unique_name(&base, &taken);
        assert!(next.len() <= MAX_NAME, "`{next}` is {} long", next.len());
        assert!(
            validate_name(&next).is_ok(),
            "{next}: {:?}",
            validate_name(&next)
        );
        assert!(next.ends_with("-2"), "{next}");
    }

    /// Shortening the stem can leave it ending in a dash, which the gateway's
    /// name rules reject.
    #[test]
    fn the_shortened_stem_never_ends_in_a_dash() {
        // 13 characters with a dash where the truncation lands.
        let base = "aaaaaaaaaaaa-b";
        let taken = vec![base.to_string()];
        let next = unique_name(base, &taken);
        assert!(validate_name(&next).is_ok(), "{next}");
        assert!(!next.contains("--"), "{next}");
    }

    #[test]
    fn slugifies_task_text() {
        assert_eq!(
            slugify("Add OAuth login!").as_deref(),
            Some("add-oauth-login")
        );
        assert_eq!(slugify("  fix   the BUG  ").as_deref(), Some("fix-bug"));
        assert_eq!(slugify("!!!"), None);
        assert_eq!(slugify(""), None);
    }

    /// The name that started this: fifteen characters of "I want to add the
    /// MaxGaming Scala customer id" was `i-want-to-add`, which says nothing
    /// about the task at all.
    #[test]
    fn filler_words_do_not_get_to_spend_the_budget() {
        assert_eq!(
            slugify("I want to add the MaxGaming Scala customer id").as_deref(),
            Some("add-maxgaming-scala-customer-id")
        );
        assert_eq!(
            slugify("Can you please update the changelog for me").as_deref(),
            Some("update-changelog")
        );
        // Verbs stay: they are what tells two tasks about one subject apart.
        assert_eq!(slugify("remove the flag").as_deref(), Some("remove-flag"));
        assert_eq!(slugify("add the flag").as_deref(), Some("add-flag"));
    }

    /// A task made entirely of filler still has to produce a name.
    #[test]
    fn a_task_of_nothing_but_filler_falls_back_to_the_words_it_has() {
        assert_eq!(
            slugify("can you do it for me").as_deref(),
            Some("can-you-do-it-for-me")
        );
    }

    /// Long enough to say what the session is, and still a legal branch, label
    /// and list column.
    #[test]
    fn a_name_may_be_longer_than_a_sandbox_name() {
        let long = "add-maxgaming-scala-customer-id-to-prod";
        assert!(long.len() > MAX_NAME_IN_SANDBOX && long.len() <= MAX_NAME);
        assert_eq!(validate_name(long), Ok(()));

        let s = Session::new(long.into(), "https://example.com/r.git".into(), "t".into());
        assert_eq!(s.work_branch, format!("sbx/{long}"));
        assert_eq!(
            s.labels().get(LABEL_SESSION).map(String::as_str),
            Some(long),
            "the full name lives in the label, whatever the sandbox is called"
        );
    }

    /// The derived sandbox name is a pure function of the session name -- that
    /// is what lets `sbx rm` and adoption name a sandbox with no record to read.
    #[test]
    fn a_long_name_gets_a_short_sandbox_of_its_own() {
        let a = "maxgaming-scala-customer-id";
        let b = "maxgaming-scala-tax-rate";

        for name in [a, b] {
            let sandbox = sandbox_name(name);
            assert!(
                sandbox.len() <= MAX_SANDBOX_NAME,
                "`{sandbox}` is {} long",
                sandbox.len()
            );
            assert!(sandbox.starts_with("sbx-maxgaming"), "{sandbox}");
            assert_eq!(sandbox, sandbox_name(name), "must be deterministic");
        }
        assert_ne!(
            sandbox_name(a),
            sandbox_name(b),
            "two names sharing fifteen characters must not share a sandbox"
        );

        // Short names are untouched, so sandboxes created before this keep the
        // names they already have.
        assert_eq!(sandbox_name("add-auth"), "sbx-add-auth");
        assert_eq!(
            sandbox_name(&"a".repeat(MAX_NAME_IN_SANDBOX)).len(),
            MAX_SANDBOX_NAME
        );
    }

    /// A stem cut mid-name must not leave the dash next to the discriminator.
    #[test]
    fn a_shortened_sandbox_name_never_doubles_its_dash() {
        // Ten characters of this land exactly on a dash.
        let sandbox = sandbox_name("fix-thing-with-a-long-tail");
        assert!(!sandbox.contains("--"), "{sandbox}");
        assert!(sandbox.len() <= MAX_SANDBOX_NAME);
    }

    #[test]
    fn derives_a_name_from_the_task_then_the_repo() {
        assert_eq!(
            derive_name("Fix the README typo", "https://github.com/o/r.git").as_deref(),
            Some("fix-readme-typo")
        );
        // No task: the repository's own name is the next best thing.
        assert_eq!(
            derive_name("", "https://github.com/o/hello-world.git").as_deref(),
            Some("hello-world")
        );
        assert_eq!(
            derive_name("", "https://dev.azure.com/org/proj/_git/repo").as_deref(),
            Some("repo")
        );
        assert_eq!(derive_name("", "!!!"), None);
    }

    #[test]
    fn knows_which_provider_type_carries_the_agent_credential() {
        assert_eq!(agent_provider_type("claude"), Some("claude-code-oauth"));
        assert_eq!(agent_provider_type("codex"), None);
    }

    #[test]
    fn rejects_names_the_gateway_would_reject() {
        assert_eq!(validate_name(""), Err(NameError::Empty));
        assert_eq!(validate_name("Add-Auth"), Err(NameError::BadChar('A')));
        assert_eq!(validate_name("add/auth"), Err(NameError::BadChar('/')));
        assert_eq!(validate_name("-auth"), Err(NameError::BadEdge));
        assert_eq!(validate_name("auth-"), Err(NameError::BadEdge));
        assert_eq!(
            validate_name(&"a".repeat(MAX_NAME + 1)),
            Err(NameError::TooLong)
        );
        assert!(validate_name("add-auth-2").is_ok());
    }

    #[test]
    fn derives_consistent_identifiers() {
        let s = Session::new(
            "add-auth".into(),
            "https://example.com/r.git".into(),
            "task".into(),
        );
        assert_eq!(s.sandbox, "sbx-add-auth");
        assert_eq!(s.tmux, TMUX_SESSION);
        assert_eq!(s.work_branch, "sbx/add-auth");
        let l = s.labels();
        assert_eq!(l.get(LABEL_SESSION).map(String::as_str), Some("add-auth"));
        assert_eq!(l.get(LABEL_MANAGED).map(String::as_str), Some("true"));
        // Every label value must satisfy the gateway's rules.
        for v in l.values() {
            assert!(v.len() <= MAX_LABEL_VALUE);
            assert!(
                v.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
            );
        }
    }

    #[test]
    fn state_display_honours_a_width() {
        assert_eq!(format!("{:<9}|", State::Ready), "ready    |");
        assert_eq!(format!("{:<9}|", State::Published), "published|");
        assert_eq!(format!("{}", State::Waiting), "waiting");
    }

    #[test]
    fn humanizes_age() {
        assert_eq!(humanize_age(0, 30), "30s");
        assert_eq!(humanize_age(0, 300), "5m");
        assert_eq!(humanize_age(0, 7200), "2h");
        assert_eq!(humanize_age(0, 172_800), "2d");
        // Clock skew must not panic.
        assert_eq!(humanize_age(100, 0), "0s");
    }

    #[test]
    fn session_json_roundtrips() {
        let s = Session::new("x".into(), "r".into(), "t".into());
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Session>(&json).unwrap(), s);
    }
}
