//! The secrets a managed MCP server is given, held on the server and nowhere
//! else.
//!
//! An MCP server worth having needs a credential: a Jira token, a Sentry token,
//! an Azure DevOps PAT. Those used to live in whatever `docker run -e` line
//! started the container by hand, which meant they lived in a shell history and
//! in somebody's notes. Now `sbxd` owns the containers, so it has to own the
//! secrets too.
//!
//! **A value goes in and never comes back out.** The protocol can set one,
//! forget one, and list the *names*; there is no request that returns a value,
//! and there will not be one. A client showing a token would be a token on
//! another machine, in a webview's memory, and in whatever that machine backs
//! up -- for the sole benefit of confirming what was typed. What a client can
//! ask is whether a name is set, which is the question that actually gets asked.
//!
//! This is not encryption at rest. The file is 0600 in the server's state
//! directory, which is the same protection the pairing tokens and the TLS key
//! next to it have: anyone who can read it can already run commands as the user
//! that would use it. Encrypting it with a key stored beside it would be theatre
//! -- worth saying out loud, because a file called `secrets.json` invites the
//! assumption that something clever is happening.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// `$XDG_STATE_HOME/sbx/secrets.json`, beside the tokens and the key.
pub fn path() -> PathBuf {
    crate::state::dir().join("secrets.json")
}

/// One name, and whether there is a value behind it.
///
/// What a client is told, in full. The `used_by` is what makes the screen
/// useful: a secret nobody references is either a leftover or a typo in a
/// catalog entry, and both are worth seeing.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, rename = "NamedSecret"))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Named {
    pub name: String,
    /// Always true for a stored secret. Present because the same shape carries
    /// a name a catalog entry asks for and the store does not have -- which is
    /// the failure this screen exists to make visible.
    pub set: bool,
    /// The MCP servers whose catalog entry names it.
    pub used_by: Vec<String>,
}

/// Everything the store holds. A map, so the file reads as what it is.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    secrets: BTreeMap<String, String>,
}

fn load_from(path: &Path) -> io::Result<Stored> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(io::Error::other),
        // No file is the normal first-run case.
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Stored::default()),
        Err(e) => Err(e),
    }
}

fn save_to(path: &Path, stored: &Stored) -> io::Result<()> {
    let json = serde_json::to_string_pretty(stored).map_err(io::Error::other)?;
    crate::state::write_private(path, &json)
}

/// The names in the store, sorted. Never the values.
pub fn names() -> Vec<String> {
    names_at(&path())
}

pub fn names_at(path: &Path) -> Vec<String> {
    load_from(path)
        .map(|s| s.secrets.into_keys().collect())
        .unwrap_or_default()
}

/// One value, for the one caller that may have it: the thing starting a
/// container on this machine.
///
/// `pub(crate)` deliberately. It is not reachable from `sbxd`, from the CLI or
/// from any request handler, so there is no path from a client to a value --
/// enforced by the compiler rather than by everyone remembering.
pub(crate) fn get(name: &str) -> Option<String> {
    get_at(&path(), name)
}

pub(crate) fn get_at(path: &Path, name: &str) -> Option<String> {
    load_from(path).ok()?.secrets.get(name).cloned()
}

/// Store a value under a name, replacing whatever was there.
pub fn set(name: &str, value: &str) -> Result<(), String> {
    set_at(&path(), name, value)
}

pub fn set_at(path: &Path, name: &str, value: &str) -> Result<(), String> {
    let name = validate(name)?;
    if value.is_empty() {
        // Not the same as forgetting it: an empty value would start a container
        // with the variable set to nothing, which fails as an authentication
        // error rather than as a missing secret.
        return Err("that would store an empty value; use forget to remove it".into());
    }
    let mut stored =
        load_from(path).map_err(|e| format!("could not read the secret store: {e}"))?;
    stored.secrets.insert(name, value.to_string());
    save_to(path, &stored).map_err(|e| format!("could not write the secret store: {e}"))
}

/// Drop a name and its value. Removing one that is not there is the desired end
/// state rather than a failure.
pub fn forget(name: &str) -> Result<(), String> {
    forget_at(&path(), name)
}

pub fn forget_at(path: &Path, name: &str) -> Result<(), String> {
    let mut stored =
        load_from(path).map_err(|e| format!("could not read the secret store: {e}"))?;
    stored.secrets.remove(name.trim());
    save_to(path, &stored).map_err(|e| format!("could not write the secret store: {e}"))
}

/// An environment variable name, and nothing else.
///
/// Checked because the name is interpolated into a `docker run -e` argument: a
/// name with a `=` in it would set a different variable to a different value
/// than the one the catalog says, which is the kind of mistake that produces a
/// container holding a secret nobody meant to give it.
fn validate(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("a secret needs a name".into());
    }
    if let Some(c) = name
        .chars()
        .find(|c| !c.is_ascii_alphanumeric() && *c != '_')
    {
        return Err(format!(
            "`{name}` is not an environment variable name; `{c}` is not allowed"
        ));
    }
    if name.starts_with(|c: char| c.is_ascii_digit()) {
        return Err(format!("`{name}` starts with a digit"));
    }
    Ok(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let p =
            std::env::temp_dir().join(format!("sbx-secrets-{name}-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn a_value_goes_in_and_only_its_name_comes_out() {
        let p = scratch("roundtrip");
        set_at(&p, "SENTRY_TOKEN", "sntrys_abc").unwrap();
        set_at(&p, "JIRA_TOKEN", "atl_def").unwrap();

        // The names, in a stable order, and nothing else. `names` is the only
        // way out of this module for anything a client can see.
        assert_eq!(names_at(&p), ["JIRA_TOKEN", "SENTRY_TOKEN"]);
        assert_eq!(get_at(&p, "SENTRY_TOKEN").as_deref(), Some("sntrys_abc"));

        // And the file itself says nothing about what it is protecting beyond
        // being unreadable to anyone else.
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("SENTRY_TOKEN"), "{text}");

        forget_at(&p, "SENTRY_TOKEN").unwrap();
        assert_eq!(names_at(&p), ["JIRA_TOKEN"]);
        assert_eq!(get_at(&p, "SENTRY_TOKEN"), None);
        // Twice is fine: the end state is what was asked for.
        forget_at(&p, "SENTRY_TOKEN").unwrap();
        let _ = std::fs::remove_file(&p);
    }

    /// The name is interpolated into a `docker run -e` argument, so a name that
    /// is not a variable name is refused where it is typed rather than where it
    /// would take effect.
    #[test]
    fn a_name_that_is_not_an_environment_variable_is_refused() {
        let p = scratch("names");
        for bad in ["", "  ", "A=B", "A B", "TOKEN;rm -rf /", "1TOKEN", "a-b"] {
            assert!(set_at(&p, bad, "v").is_err(), "`{bad}` was accepted");
        }
        assert!(set_at(&p, " OK_TOKEN2 ", "v").is_ok());
        assert_eq!(names_at(&p), ["OK_TOKEN2"], "the name is trimmed");
        let _ = std::fs::remove_file(&p);
    }

    /// Storing nothing is not how a secret is removed, and treating it as such
    /// would leave a container with an empty variable -- which fails as an
    /// authentication error, a long way from the cause.
    #[test]
    fn an_empty_value_is_refused_rather_than_treated_as_forget() {
        let p = scratch("empty");
        set_at(&p, "TOKEN", "v").unwrap();
        assert!(set_at(&p, "TOKEN", "").is_err());
        assert_eq!(get_at(&p, "TOKEN").as_deref(), Some("v"));
        let _ = std::fs::remove_file(&p);
    }

    /// It lives with the keys and the tokens, not with the session cache: the
    /// config directory is the one people copy between machines.
    #[test]
    fn it_lives_in_the_state_directory() {
        let p = path();
        assert!(p.ends_with("secrets.json"), "{p:?}");
        assert_eq!(p.parent(), Some(crate::state::dir().as_path()));
    }
}
