//! Defaults read from a file, so they stop being flags on every command.
//!
//! `$XDG_CONFIG_HOME/sbx/config.toml`, beside the session cache and the events
//! history. Everything in it is optional and everything in it is a *default*:
//! a flag on the command line always wins, and so does an explicit choice in the
//! TUI's create form. The file is what stops `--policy feature-work --provider
//! claude-oauth --provider azure-pat` from being typed out again every time.
//!
//! **A file that cannot be read is an error, not a shrug.** A typo'd key or a
//! misspelled policy name that silently did nothing would be the same failure as
//! a gateway reporting a policy it is not enforcing: the tool would say one thing
//! and do another. So unknown keys are rejected, a policy name that is not a
//! template is rejected, and every command except `sbx doctor` refuses to run
//! until the file is fixed. `doctor` is the command you reach for when something
//! is wrong, so it reports the error as a failed check and carries on with the
//! built-in defaults.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::mcp;
use crate::policy;
use crate::skills;
use crate::store::Store;

/// A starter file, written by `sbx config --init`.
///
/// Every key commented out and set to what the tool already does, so the file
/// documents the defaults rather than changing them the moment it is created.
pub const EXAMPLE: &str = include_str!("config.example.toml");

/// Bounds on `refresh`. Below the floor the TUI's 100ms input tick becomes the
/// limit -- the polls would land faster than the frames that draw them, which
/// buys nothing and costs a `git status` inside every sandbox; above the ceiling
/// the list has stopped being live and the tool would be better closed.
const REFRESH_MIN: Duration = Duration::from_millis(250);
const REFRESH_MAX: Duration = Duration::from_secs(60);

/// The defaults, and where they came from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// The file this was read from. Present even when the file does not exist,
    /// because `sbx config` has to be able to say where to create it.
    pub path: PathBuf,
    /// Whether the file existed. The difference between "these are the defaults"
    /// and "this is what you asked for".
    pub present: bool,

    /// Gateway to talk to, when `--gateway` does not say.
    pub gateway: Option<String>,
    /// Repository a `sbx new` without `--repo` clones, and the row the TUI's
    /// picker opens on.
    pub repo: Option<String>,
    /// Branch to clone from. `None` means the remote's default.
    pub base: Option<String>,
    /// Policy template name, or a path to a YAML file.
    pub policy: Option<String>,
    /// Credential providers attached to a new session.
    pub providers: Option<Vec<String>>,
    /// Where the TUI's picker looks for repositories. Replaces the built-in
    /// roots rather than adding to them, like `SBX_REPO_ROOTS`, which still wins.
    pub repo_roots: Option<Vec<PathBuf>>,
    /// Where worktree sessions put their working copies, one directory each.
    ///
    /// Server-side, like [`Self::repo_roots`] and for the same reason: the
    /// machine that adds the worktree is the one that has the checkout. `None`
    /// means [`crate::backend::Worktree::default_root`].
    pub worktree_root: Option<PathBuf>,
    /// How often the TUI reads the sandboxes. See its `Intervals`:
    /// this is one number scaling a set of measured ones, because they are
    /// related to each other and a single absolute interval would break the
    /// relationships.
    pub refresh: Option<Duration>,
    /// Skills copied into every new session, already resolved to host paths.
    ///
    /// Global for the same reason as [`Self::mcp`]: this is what an agent of
    /// yours knows how to do, not a per-session choice.
    pub skills: Vec<skills::Skill>,
    /// MCP servers every new session's agent is given, already validated.
    ///
    /// Not a per-session choice and so not on [`crate::ops::Draft`]: like the
    /// global endpoint lists, this is one decision about what an agent of yours
    /// can reach, made once. A session records the servers it was created with,
    /// so changing the file changes the next session rather than this one.
    pub mcp: Vec<mcp::Server>,
}

impl Config {
    /// `$XDG_CONFIG_HOME/sbx/config.toml`, beside the session cache.
    pub fn default_path() -> PathBuf {
        Store::default_path().with_file_name("config.toml")
    }

    pub fn load() -> Result<Self, Error> {
        Self::load_from(Self::default_path())
    }

    pub fn load_from(path: impl Into<PathBuf>) -> Result<Self, Error> {
        let path = path.into();
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            // No file is the normal case: the defaults are meant to be good.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(Config {
                    path,
                    ..Config::default()
                });
            }
            Err(source) => return Err(Error::Read { path, source }),
        };
        Self::parse(&path, &text)
    }

    /// Parse and validate. Separate from the read so the tests need no files.
    pub fn parse(path: &Path, text: &str) -> Result<Self, Error> {
        let mut raw: Raw = toml::from_str(text).map_err(|e| Error::Parse {
            path: path.to_path_buf(),
            // toml's own message already carries the line, the column and a
            // caret; reprinting it is better than summarising it.
            message: e.to_string().trim_end().to_string(),
        })?;

        // Blanks become unset before anything is validated, so `policy = ""`
        // means "not decided" rather than "a template with no name".
        raw.gateway = blank_to_none(raw.gateway);
        raw.repo = blank_to_none(raw.repo);
        raw.base = blank_to_none(raw.base);
        raw.policy = blank_to_none(raw.policy);

        let invalid = |key: &'static str, message: String| Error::Invalid {
            path: path.to_path_buf(),
            key,
            message,
        };

        // A policy that is neither a template nor a path is a typo, and finding
        // out at create time -- after a sandbox exists -- is finding out late.
        if let Some(spec) = &raw.policy
            && !looks_like_path(spec)
            && policy::find(spec).is_none()
        {
            return Err(invalid(
                "policy",
                format!(
                    "`{spec}` is not a template; expected one of {}, or a path to a YAML file",
                    names()
                ),
            ));
        }

        let refresh = match &raw.refresh {
            None => None,
            Some(raw) => {
                let d = parse_duration(raw).map_err(|m| invalid("refresh", m))?;
                if d < REFRESH_MIN || d > REFRESH_MAX {
                    return Err(invalid("refresh", format!("`{raw}` is outside 250ms..60s")));
                }
                Some(d)
            }
        };

        // An empty list is a mistake worth naming: `providers = []` reads as
        // "no credentials", which produces a sandbox whose agent cannot log in
        // and whose clone cannot authenticate, three steps later.
        if raw.providers.as_ref().is_some_and(Vec::is_empty) {
            return Err(invalid(
                "providers",
                "is empty; remove the key to accept the defaults".to_string(),
            ));
        }
        if raw.repo_roots.as_ref().is_some_and(Vec::is_empty) {
            return Err(invalid(
                "repo_roots",
                "is empty; remove the key to scan the usual places".to_string(),
            ));
        }

        // Resolved but not checked against the filesystem: a skill directory
        // that is temporarily gone -- a repository not cloned yet, an external
        // drive not mounted -- should not stop every command from running. The
        // shape is checked here, existence is `sbx doctor`'s to report and a
        // create-time warning otherwise.
        let mut resolved_skills = Vec::new();
        for entry in raw.skills.iter().flatten() {
            let skill = skills::Skill::parse(entry)
                .map_err(|e| invalid("skills", format!("`{entry}` {e}")))?;
            if resolved_skills
                .iter()
                .any(|s: &skills::Skill| s.name == skill.name)
            {
                return Err(invalid(
                    "skills",
                    format!(
                        "`{}` is the name of two skills; the agent keys them by \
                         directory name, so one would hide the other",
                        skill.name
                    ),
                ));
            }
            resolved_skills.push(skill);
        }
        if raw.skills.as_ref().is_some_and(Vec::is_empty) {
            return Err(invalid(
                "skills",
                "is empty; remove the key to copy none".to_string(),
            ));
        }

        // Validated here, where the error can name the file and the entry,
        // rather than at create time: a URL the sandbox cannot reach is worth
        // finding out about before a sandbox exists, and a loopback URL -- the
        // one mistake everybody makes -- looks perfectly fine until the agent
        // is running.
        let mut mcp = Vec::new();
        for entry in raw.mcp.into_iter().flatten() {
            let transport = match &entry.transport {
                Some(t) => mcp::Transport::parse(t)
                    .map_err(|e| invalid("mcp", format!("`{}`: {e}", entry.name)))?,
                None => mcp::Transport::default(),
            };
            let server = mcp::Server::parse(&entry.name, &entry.url, transport)
                .map_err(|e| invalid("mcp", format!("`{}`: {e}", entry.name)))?;
            // The agent keys its servers by name, so two entries sharing one
            // means the second silently replaces the first.
            if mcp.iter().any(|s: &mcp::Server| s.name == server.name) {
                return Err(invalid(
                    "mcp",
                    mcp::Error::DuplicateName(server.name).to_string(),
                ));
            }
            mcp.push(server);
        }

        Ok(Config {
            path: path.to_path_buf(),
            present: true,
            gateway: raw.gateway,
            repo: raw.repo,
            base: raw.base,
            policy: raw.policy,
            providers: raw.providers,
            repo_roots: raw
                .repo_roots
                .map(|list| list.iter().map(|p| expand_tilde(p)).collect()),
            worktree_root: raw.worktree_root.as_deref().map(expand_tilde),
            refresh,
            skills: resolved_skills,
            mcp,
        })
    }

    /// The policy a new session gets when nothing else says.
    pub fn policy(&self) -> &str {
        self.policy.as_deref().unwrap_or(policy::DEFAULT_TEMPLATE)
    }

    /// The providers a new session gets when nothing else says.
    pub fn providers(&self) -> &[String] {
        self.providers.as_deref().unwrap_or(&[])
    }

    /// The MCP servers a new session's agent is given.
    pub fn mcp(&self) -> &[mcp::Server] {
        &self.mcp
    }

    /// The skills copied into a new session.
    pub fn skills(&self) -> &[skills::Skill] {
        &self.skills
    }
}

/// Exactly the file's shape, so serde rejects anything else.
///
/// Separate from [`Config`] because the two are not the same type: the file
/// holds `refresh = "2s"` and a config holds a [`Duration`], and putting the
/// validation in between is what lets the errors name the key.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    gateway: Option<String>,
    repo: Option<String>,
    base: Option<String>,
    policy: Option<String>,
    providers: Option<Vec<String>>,
    repo_roots: Option<Vec<PathBuf>>,
    worktree_root: Option<PathBuf>,
    refresh: Option<String>,
    skills: Option<Vec<String>>,
    /// `[[mcp]]` tables. An `Option` so `deny_unknown_fields` still rejects a
    /// misspelled `[[mcps]]` rather than reading it as none configured.
    mcp: Option<Vec<RawMcp>>,
}

/// One `[[mcp]]` table, before it is checked. Its own struct so a misspelled key
/// inside one is an error too, and so the message can name the entry.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMcp {
    name: String,
    url: String,
    transport: Option<String>,
}

#[derive(Debug)]
pub enum Error {
    Read {
        path: PathBuf,
        source: io::Error,
    },
    Parse {
        path: PathBuf,
        message: String,
    },
    Invalid {
        path: PathBuf,
        key: &'static str,
        message: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Read { path, source } => {
                write!(f, "could not read {}: {source}", path.display())
            }
            Error::Parse { path, message } => {
                write!(f, "{}: {message}", path.display())
            }
            Error::Invalid { path, key, message } => {
                write!(f, "{}: `{key}` {message}", path.display())
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Read { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// The same rule `policy::resolve` uses, so validation and resolution cannot
/// disagree about what is a path.
fn looks_like_path(spec: &str) -> bool {
    spec.contains('/') || spec.ends_with(".yaml") || spec.ends_with(".yml")
}

fn names() -> String {
    policy::TEMPLATES
        .iter()
        .map(|t| t.name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// `""` means unset, not "a value that happens to be empty".
///
/// A key left as `policy = ""` while editing is far more likely to mean "I have
/// not decided" than "use a policy with no name", and the latter is only ever
/// an error two steps later.
fn blank_to_none(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.trim().is_empty())
}

/// `~` and `~/...`, which is how anyone writes a home-relative path in a config
/// file and which the shell is not around to expand here.
fn expand_tilde(path: &Path) -> PathBuf {
    let Some(rest) = path.to_str().and_then(|s| s.strip_prefix('~')) else {
        return path.to_path_buf();
    };
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return path.to_path_buf();
    };
    match rest.strip_prefix('/') {
        Some(tail) => home.join(tail),
        None if rest.is_empty() => home,
        // `~other/...` is another user's home, which is not ours to guess at.
        None => path.to_path_buf(),
    }
}

/// A whole number of milliseconds or seconds.
///
/// The unit is required. `refresh = 2` could plausibly mean either, and a config
/// that polls two thousand times too often is worse than one that refuses to
/// load.
fn parse_duration(raw: &str) -> Result<Duration, String> {
    let t = raw.trim();
    // `ms` first: every `ms` also ends in `s`.
    let (digits, per) = if let Some(n) = t.strip_suffix("ms") {
        (n, 1u64)
    } else if let Some(n) = t.strip_suffix('s') {
        (n, 1000)
    } else {
        return Err(format!("`{raw}` has no unit; write it as `500ms` or `2s`"));
    };
    let n: u64 = digits
        .trim()
        .parse()
        .map_err(|_| format!("`{raw}` is not a whole number of milliseconds or seconds"))?;
    n.checked_mul(per)
        .map(Duration::from_millis)
        .ok_or_else(|| format!("`{raw}` is too large"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Config, Error> {
        Config::parse(Path::new("/tmp/config.toml"), text)
    }

    #[test]
    fn an_empty_file_is_all_defaults() {
        let c = parse("").unwrap();
        assert_eq!(c.policy(), policy::DEFAULT_TEMPLATE);
        assert!(c.providers().is_empty());
        assert_eq!(c.refresh, None);
        assert!(c.present, "the file existed, it was just empty");
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let c = Config::load_from("/nonexistent/sbx/config.toml").unwrap();
        assert!(!c.present);
        assert_eq!(c.policy(), policy::DEFAULT_TEMPLATE);
    }

    #[test]
    fn reads_every_key() {
        let c = parse(
            r#"
            gateway = "work"
            repo = "https://github.com/o/r"
            base = "develop"
            policy = "readonly-explore"
            providers = ["claude-oauth", "azure-pat"]
            repo_roots = ["/srv/code"]
            refresh = "2s"
            "#,
        )
        .unwrap();
        assert_eq!(c.gateway.as_deref(), Some("work"));
        assert_eq!(c.repo.as_deref(), Some("https://github.com/o/r"));
        assert_eq!(c.base.as_deref(), Some("develop"));
        assert_eq!(c.policy(), "readonly-explore");
        assert_eq!(c.providers(), ["claude-oauth", "azure-pat"]);
        assert_eq!(c.repo_roots.unwrap(), [PathBuf::from("/srv/code")]);
        assert_eq!(c.refresh, Some(Duration::from_secs(2)));
    }

    /// The whole point of `deny_unknown_fields`: a key that does nothing is
    /// indistinguishable from a key that is not working.
    #[test]
    fn an_unknown_key_is_an_error() {
        let e = parse("polciy = \"feature-work\"").unwrap_err();
        assert!(
            e.to_string().contains("polciy"),
            "the message should name the key: {e}"
        );
    }

    #[test]
    fn a_policy_that_is_not_a_template_is_an_error() {
        let e = parse(r#"policy = "feature-wrok""#).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("feature-wrok"), "{msg}");
        assert!(
            msg.contains(policy::DEFAULT_TEMPLATE),
            "lists the real ones: {msg}"
        );
    }

    /// A path is not checked for existence here: it may be relative to wherever
    /// the command is run, and `policy::resolve` gives the better error.
    #[test]
    fn a_policy_path_is_taken_on_trust() {
        assert_eq!(
            parse(r#"policy = "./my.yaml""#).unwrap().policy(),
            "./my.yaml"
        );
        assert_eq!(
            parse(r#"policy = "/etc/sbx/strict.yaml""#)
                .unwrap()
                .policy(),
            "/etc/sbx/strict.yaml"
        );
    }

    #[test]
    fn refresh_needs_a_unit() {
        let e = parse(r#"refresh = "2""#).unwrap_err();
        assert!(e.to_string().contains("no unit"), "{e}");
        assert_eq!(
            parse(r#"refresh = "750ms""#).unwrap().refresh,
            Some(Duration::from_millis(750))
        );
    }

    #[test]
    fn refresh_is_bounded() {
        assert!(parse(r#"refresh = "1ms""#).is_err());
        assert!(
            parse(r#"refresh = "100ms""#).is_err(),
            "below the input tick"
        );
        assert!(
            parse(r#"refresh = "5m""#).is_err(),
            "minutes are not a unit here"
        );
        assert!(parse(r#"refresh = "120s""#).is_err());
        assert!(
            parse(r#"refresh = "250ms""#).is_ok(),
            "the floor is inclusive"
        );
        assert!(parse(r#"refresh = "60s""#).is_ok(), "so is the ceiling");
    }

    #[test]
    fn an_empty_list_is_an_error_rather_than_a_silent_nothing() {
        assert!(
            parse("providers = []")
                .unwrap_err()
                .to_string()
                .contains("providers")
        );
        assert!(
            parse("repo_roots = []")
                .unwrap_err()
                .to_string()
                .contains("repo_roots")
        );
    }

    #[test]
    fn a_blank_string_is_unset() {
        let c = parse(
            r#"policy = ""
                         gateway = "  ""#,
        )
        .unwrap();
        assert_eq!(c.policy(), policy::DEFAULT_TEMPLATE);
        assert_eq!(c.gateway, None);
    }

    #[test]
    fn tilde_is_expanded_against_home() {
        // SAFETY: single-threaded test process for this variable; the rest of
        // the suite reads HOME only through this helper.
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let Some(home) = home else { return };
        let c = parse(r#"repo_roots = ["~/dev", "~", "/abs", "~other/x"]"#).unwrap();
        assert_eq!(
            c.repo_roots.unwrap(),
            [
                home.join("dev"),
                home,
                PathBuf::from("/abs"),
                PathBuf::from("~other/x"),
            ]
        );
    }

    /// The shipped example has to parse, or `--init` writes a file that stops
    /// every later command from running.
    #[test]
    fn the_example_file_parses() {
        let c = parse(EXAMPLE).expect("the example must be valid");
        assert_eq!(
            c.policy(),
            policy::DEFAULT_TEMPLATE,
            "every key in the example is commented out, so it changes nothing"
        );
        assert_eq!(c.refresh, None);
    }

    /// Every key the example mentions has to be a key the parser accepts, or the
    /// documentation and the code drift apart silently.
    #[test]
    fn the_example_documents_every_key() {
        for key in [
            "gateway",
            "repo",
            "base",
            "policy",
            "providers",
            "repo_roots",
            "worktree_root",
            "refresh",
            "skills",
        ] {
            assert!(
                EXAMPLE.contains(&format!("# {key} =")),
                "the example does not show `{key}`"
            );
        }
        // A table rather than a key, so it is shown as one.
        assert!(
            EXAMPLE.contains("# [[mcp]]"),
            "the example does not show `[[mcp]]`"
        );
    }

    #[test]
    fn skills_take_a_name_or_a_path() {
        let c = parse(r#"skills = ["ship-pr", "~/dev/sbx/.claude/skills/audit"]"#).unwrap();
        assert_eq!(c.skills().len(), 2);
        assert_eq!(c.skills()[0].name, "ship-pr");
        assert_eq!(
            c.skills()[0].source,
            crate::skills::host_skills_dir().join("ship-pr"),
            "a bare name is one of your own"
        );
        assert_eq!(c.skills()[1].name, "audit");
    }

    /// A skill directory that is not there is not a config error: it may be a
    /// repository that is not cloned yet, and every command refusing to run over
    /// it would be worse than the session missing the skill.
    #[test]
    fn a_skill_that_does_not_exist_still_loads() {
        let c = parse(r#"skills = ["/nope/not/here"]"#).unwrap();
        assert_eq!(c.skills()[0].name, "here");
        assert!(c.skills()[0].problem().is_some(), "doctor's to report");
    }

    #[test]
    fn two_skills_cannot_share_a_directory_name() {
        let e = parse(r#"skills = ["/a/ship-pr", "/b/ship-pr"]"#).unwrap_err();
        assert!(e.to_string().contains("two skills"), "{e}");
    }

    #[test]
    fn an_empty_skills_list_is_an_error() {
        assert!(
            parse("skills = []")
                .unwrap_err()
                .to_string()
                .contains("skills")
        );
    }

    #[test]
    fn mcp_servers_are_read_and_defaulted() {
        let c = parse(
            r#"
            [[mcp]]
            name = "jira"
            url = "http://mcp-atlassian:9000/mcp"

            [[mcp]]
            name = "azure-devops"
            url = "http://mcp-azure:9001/sse"
            transport = "sse"
            "#,
        )
        .unwrap();
        assert_eq!(c.mcp().len(), 2);
        assert_eq!(c.mcp()[0].endpoint, "mcp-atlassian:9000");
        assert_eq!(
            c.mcp()[0].transport,
            mcp::Transport::Http,
            "http is the transport a server has unless it says otherwise"
        );
        assert_eq!(c.mcp()[1].transport, mcp::Transport::Sse);
    }

    #[test]
    fn no_mcp_table_is_no_servers() {
        assert!(parse("").unwrap().mcp().is_empty());
    }

    /// The mistake this validation exists for: correct on the host, wrong in the
    /// sandbox, and invisible until the agent is running.
    #[test]
    fn a_loopback_mcp_url_is_refused_by_name() {
        let e = parse(
            r#"
            [[mcp]]
            name = "jira"
            url = "http://localhost:9000/mcp"
            "#,
        )
        .unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("jira"), "names the entry: {msg}");
        assert!(
            msg.contains("host.openshell.internal"),
            "says what to use: {msg}"
        );
    }

    #[test]
    fn a_misspelled_mcp_key_is_an_error() {
        let e = parse(
            r#"
            [[mcp]]
            name = "jira"
            urls = "http://mcp:9000/mcp"
            "#,
        )
        .unwrap_err();
        assert!(e.to_string().contains("urls"), "{e}");
    }

    /// The agent keys its servers by name, so a duplicate is one server
    /// silently replacing another.
    #[test]
    fn two_mcp_servers_cannot_share_a_name() {
        let e = parse(
            r#"
            [[mcp]]
            name = "jira"
            url = "http://a:9000/mcp"

            [[mcp]]
            name = "jira"
            url = "http://b:9000/mcp"
            "#,
        )
        .unwrap_err();
        assert!(e.to_string().contains("already the name"), "{e}");
    }
}
