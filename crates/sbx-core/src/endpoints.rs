//! The global allow and block lists.
//!
//! `w` and `t` change one session's egress and forget it. These are the same
//! decision made once: an endpoint on the allow list is opened on every
//! `sbx new`, and one on the block list is closed on every `sbx new` --
//! including endpoints the policy template itself grants, which is the only
//! thing a block list *can* mean under an engine that denies by default.
//!
//! That asymmetry is worth being explicit about, because "blacklist" invites
//! the wrong model. OpenShell has no deny-overrides-allow layer at L4; an
//! endpoint is unreachable unless a rule names it. So blocking `pastebin.com`
//! is a no-op -- it was never reachable -- and blocking `platform.claude.com`
//! is real, because `feature-work.yaml` grants it. The pane says which.
//!
//! `$XDG_CONFIG_HOME/sbx/endpoints.json`, beside the session cache, and a cache
//! in the same sense: losing it costs the lists and nothing else. Written under
//! a lock of its own for the reason [`crate::store`] holds one -- a TUI and a
//! `sbx new` in another terminal are the normal case, not an edge, and
//! load-modify-save without a lock loses whichever write lands second.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;

use openshell_client::PolicyUpdate;

use crate::store::Store;

/// The access class an allow entry grants.
///
/// Not configurable, and deliberately the broadest one. This list is written by
/// pressing a key next to a denial that has already happened, which means the
/// answer to "what does this need?" is "whatever it was trying to do"; offering
/// a choice between `full` and `read-only` at that moment would be asking a
/// question the user is not in a position to answer. A narrower grant belongs in
/// a policy template, where it can be written down with a reason.
const ACCESS: &str = "full";

/// An endpoint opened for every new session, and what may reach it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Allow {
    /// `host:port`, which is what `policy update` addresses.
    pub endpoint: String,
    /// Kernel-resolved binary paths, as the log reported them. Never empty: an
    /// endpoint rule with no binaries grants nothing, so an allow that could not
    /// name one is refused at the point it is asked for rather than written
    /// here and silently doing nothing. See [`Lists::allow`].
    pub binaries: Vec<String>,
}

/// The lists, as they are on disk.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Lists {
    pub allow: Vec<Allow>,
    /// `host:port` for each. No binaries: removing an endpoint removes it for
    /// everything, which is the only granularity `--remove-endpoint` has.
    pub block: Vec<String>,
}

impl Lists {
    /// `$XDG_CONFIG_HOME/sbx/endpoints.json`, beside the session cache.
    pub fn default_path() -> PathBuf {
        Store::default_path().with_file_name("endpoints.json")
    }

    pub fn load() -> io::Result<Self> {
        Self::load_from(Self::default_path())
    }

    pub fn load_from(path: impl Into<PathBuf>) -> io::Result<Self> {
        match fs::read_to_string(path.into()) {
            Ok(text) => serde_json::from_str(&text).map_err(io::Error::other),
            // No file is the normal first-run case, not an error.
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Lists::default()),
            Err(e) => Err(e),
        }
    }

    /// Temp file and rename, like the session cache: an interrupted write must
    /// not truncate the lists.
    fn save_to(&self, path: &std::path::Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json)?;
        fs::rename(&tmp, path)
    }

    pub fn is_empty(&self) -> bool {
        self.allow.is_empty() && self.block.is_empty()
    }

    /// Whether the lists name an endpoint, and which way.
    pub fn verdict(&self, endpoint: &str) -> Option<Listed> {
        if self.block.iter().any(|e| e == endpoint) {
            return Some(Listed::Blocked);
        }
        self.allow
            .iter()
            .any(|a| a.endpoint == endpoint)
            .then_some(Listed::Allowed)
    }

    /// Put an endpoint on the allow list, taking it off the block list.
    ///
    /// The two are mutually exclusive by construction rather than by rule: an
    /// endpoint on both would make the answer depend on which list is consulted
    /// first, and there is no reading of "allowed and blocked" worth having.
    ///
    /// Replaces an existing entry rather than merging its binaries, so pressing
    /// the key twice against two different denials leaves the list saying what
    /// the second one said. Merging would grow a rule nobody wrote.
    pub fn allow(&mut self, endpoint: &str, binaries: Vec<String>) {
        self.block.retain(|e| e != endpoint);
        self.allow.retain(|a| a.endpoint != endpoint);
        self.allow.push(Allow {
            endpoint: endpoint.to_string(),
            binaries,
        });
        self.allow.sort_by(|a, b| a.endpoint.cmp(&b.endpoint));
    }

    /// Put an endpoint on the block list, taking it off the allow list.
    pub fn block(&mut self, endpoint: &str) {
        self.allow.retain(|a| a.endpoint != endpoint);
        if !self.block.iter().any(|e| e == endpoint) {
            self.block.push(endpoint.to_string());
            self.block.sort();
        }
    }

    /// The policy updates that impose these lists on a fresh sandbox.
    ///
    /// Usually one. More only when the allow list names endpoints with
    /// different binaries, because `--binary` applies to *every*
    /// `--add-endpoint` in an invocation -- the same constraint that makes
    /// [`crate::policy::Preset`] a single call with a merged binary list. Here
    /// the entries were written at different times against different denials,
    /// so merging them would grant each endpoint every other one's binaries.
    ///
    /// Removals ride on the first update, or on one of their own when there is
    /// nothing to add.
    pub fn updates(&self) -> Vec<PolicyUpdate> {
        // Grouped by binary list so endpoints that share one share a call.
        // Ordered, so the plan is the same every time it is computed.
        let mut groups: BTreeMap<Vec<String>, Vec<String>> = BTreeMap::new();
        for a in &self.allow {
            groups
                .entry(a.binaries.clone())
                .or_default()
                .push(format!("{}:{ACCESS}:rest:enforce", a.endpoint));
        }

        let mut out: Vec<PolicyUpdate> = groups
            .into_iter()
            .map(|(binaries, add_endpoints)| PolicyUpdate {
                add_endpoints,
                binaries,
                // Rejected outright for a multi-endpoint update, and the
                // gateway's own derived name (`allow_pastebin_com_443`) is
                // already the clearer of the two.
                rule_name: None,
                // The next thing this sandbox does is clone a repository, so
                // returning before the rules load would be a lie.
                wait: true,
                ..Default::default()
            })
            .collect();

        if self.block.is_empty() {
            return out;
        }
        match out.first_mut() {
            Some(first) => first.remove_endpoints = self.block.clone(),
            None => out.push(PolicyUpdate {
                remove_endpoints: self.block.clone(),
                wait: true,
                ..Default::default()
            }),
        }
        out
    }
}

/// Which list an endpoint is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listed {
    Allowed,
    Blocked,
}

impl Listed {
    pub fn label(self) -> &'static str {
        match self {
            Listed::Allowed => "allowed everywhere",
            Listed::Blocked => "blocked everywhere",
        }
    }
}

/// One endpoint's worth of change to a live sandbox.
///
/// Shared by the events pane and [`Lists::updates`]'s callers so a decision
/// applied here and the same decision applied at the next `sbx new` cannot
/// drift apart.
///
/// Measured against 0.0.110: adding an endpoint an existing rule already covers
/// does not fold into that rule, it becomes a rule of its own and the CLI says
/// so on stderr -- `would grant binary '/usr/bin/curl' undeclared authorization
/// for github.com`. That is the right outcome (the binary genuinely was not
/// authorised) and it is why the pane re-reads the policy afterwards instead of
/// reporting what it asked for.
pub fn allow_update(endpoint: &str, binaries: &[String]) -> PolicyUpdate {
    PolicyUpdate {
        add_endpoints: vec![format!("{endpoint}:{ACCESS}:rest:enforce")],
        binaries: binaries.to_vec(),
        rule_name: None,
        wait: true,
        ..Default::default()
    }
}

/// Remove an endpoint from a live sandbox.
///
/// A no-op when the endpoint is not there, verified with `policy update
/// --dry-run` against 0.0.110: the CLI exits zero and the merged policy is
/// unchanged. So this never has to be guarded by a read, and blocking something
/// that was never reachable costs a round trip and says so.
pub fn block_update(endpoint: &str) -> PolicyUpdate {
    PolicyUpdate {
        remove_endpoints: vec![endpoint.to_string()],
        wait: true,
        ..Default::default()
    }
}

/// Read the lists, change them, and write them back, with the file locked
/// throughout. The reasoning is [`crate::store::update`]'s, verbatim: more than
/// one writer is the normal case -- a TUI on one screen, a `sbx new` on another
/// -- and load-modify-save without a lock loses whichever write lands second.
///
/// Takes its paths rather than deriving them from [`Lists::default_path`], so a
/// caller under test writes to a temporary file instead of to the developer's
/// own configuration. The TUI holds the real pair in `App::lists_path`.
pub fn update_at<T>(
    lists: impl Into<PathBuf>,
    lock: impl Into<PathBuf>,
    f: impl FnOnce(&mut Lists) -> T,
) -> io::Result<T> {
    let lists = lists.into();
    let lock = lock.into();
    if let Some(dir) = lock.parent() {
        fs::create_dir_all(dir)?;
    }
    let guard = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock)?;
    guard.lock()?;

    let result = (|| {
        let mut l = Lists::load_from(&lists)?;
        let out = f(&mut l);
        l.save_to(&lists)?;
        Ok(out)
    })();

    // Explicit rather than left to the drop, so the order is visible: the write
    // above has to be inside the lock.
    let _ = guard.unlock();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lists(allow: &[(&str, &[&str])], block: &[&str]) -> Lists {
        let mut l = Lists::default();
        for (e, bins) in allow {
            l.allow(e, bins.iter().map(|b| (*b).to_string()).collect());
        }
        for e in block {
            l.block(e);
        }
        l
    }

    /// An endpoint on both lists would make the answer depend on which is read
    /// first, so putting it on one takes it off the other.
    #[test]
    fn the_two_lists_never_hold_the_same_endpoint() {
        let mut l = lists(&[("pypi.org:443", &["/usr/local/bin/uv"])], &[]);
        assert_eq!(l.verdict("pypi.org:443"), Some(Listed::Allowed));

        l.block("pypi.org:443");
        assert_eq!(l.verdict("pypi.org:443"), Some(Listed::Blocked));
        assert!(l.allow.is_empty(), "the allow entry is gone, not shadowed");

        l.allow("pypi.org:443", vec!["/usr/bin/node".into()]);
        assert_eq!(l.verdict("pypi.org:443"), Some(Listed::Allowed));
        assert!(l.block.is_empty());
        assert_eq!(l.verdict("nothing.example.com:443"), None);
    }

    /// Pressing the key twice against two denials of the same endpoint must
    /// leave the list saying what the second one said, not the union: merging
    /// grows a rule nobody wrote.
    #[test]
    fn allowing_the_same_endpoint_twice_replaces_rather_than_merges() {
        let l = lists(
            &[
                ("pastebin.com:443", &["/usr/bin/curl"]),
                ("pastebin.com:443", &["/usr/bin/wget"]),
            ],
            &[],
        );
        assert_eq!(l.allow.len(), 1);
        assert_eq!(l.allow[0].binaries, ["/usr/bin/wget"]);
    }

    /// Adding and removing in one call is what keeps a create paying for one
    /// six-second `--wait` rather than two.
    #[test]
    fn one_update_carries_every_endpoint_that_shares_a_binary_list() {
        let l = lists(
            &[
                ("pypi.org:443", &["/usr/local/bin/uv"]),
                ("files.pythonhosted.org:443", &["/usr/local/bin/uv"]),
            ],
            &["platform.claude.com:443"],
        );
        let updates = l.updates();
        assert_eq!(updates.len(), 1, "one binary list, one call");
        assert_eq!(
            updates[0].add_endpoints,
            [
                "files.pythonhosted.org:443:full:rest:enforce",
                "pypi.org:443:full:rest:enforce"
            ],
            "sorted, so the plan is the same every time it is computed"
        );
        assert_eq!(updates[0].binaries, ["/usr/local/bin/uv"]);
        assert_eq!(updates[0].remove_endpoints, ["platform.claude.com:443"]);
        assert!(updates[0].wait, "the clone runs straight after");
    }

    /// `--binary` applies to every `--add-endpoint` in the invocation, so two
    /// entries written against two different denials cannot share a call
    /// without each one gaining the other's binaries.
    #[test]
    fn endpoints_with_different_binaries_get_a_call_each() {
        let l = lists(
            &[
                ("pypi.org:443", &["/usr/local/bin/uv"]),
                ("registry.npmjs.org:443", &["/usr/bin/node"]),
            ],
            &[],
        );
        let updates = l.updates();
        assert_eq!(updates.len(), 2);
        let bins: Vec<&Vec<String>> = updates.iter().map(|u| &u.binaries).collect();
        assert!(bins.contains(&&vec!["/usr/bin/node".to_string()]));
        assert!(bins.contains(&&vec!["/usr/local/bin/uv".to_string()]));
        // Exactly one of them carries the removals, or a create would send them
        // twice.
        assert_eq!(
            updates
                .iter()
                .filter(|u| !u.remove_endpoints.is_empty())
                .count(),
            0,
            "nothing to remove here"
        );
    }

    /// A block list on its own still has to produce a call, or the endpoints a
    /// template grants would stay granted.
    #[test]
    fn a_block_list_alone_still_produces_an_update() {
        let l = lists(&[], &["platform.claude.com:443", "api.github.com:443"]);
        let updates = l.updates();
        assert_eq!(updates.len(), 1);
        assert!(updates[0].add_endpoints.is_empty());
        assert_eq!(
            updates[0].remove_endpoints,
            ["api.github.com:443", "platform.claude.com:443"]
        );
    }

    #[test]
    fn empty_lists_ask_the_gateway_for_nothing() {
        assert!(Lists::default().updates().is_empty());
        assert!(Lists::default().is_empty());
    }

    /// The file is the artifact; it has to survive a round trip, and a missing
    /// one has to read as empty rather than as an error.
    #[test]
    fn the_file_round_trips_and_a_missing_one_is_empty() {
        let dir = std::env::temp_dir().join(format!("sbx-endpoints-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("endpoints.json");
        let lock = dir.join("endpoints.lock");

        assert_eq!(Lists::load_from(&path).unwrap(), Lists::default());

        update_at(&path, &lock, |l| {
            l.allow("pastebin.com:443", vec!["/usr/bin/curl".into()]);
            l.block("platform.claude.com:443");
        })
        .unwrap();

        let read = Lists::load_from(&path).unwrap();
        assert_eq!(read.verdict("pastebin.com:443"), Some(Listed::Allowed));
        assert_eq!(
            read.verdict("platform.claude.com:443"),
            Some(Listed::Blocked)
        );
        assert_eq!(read.allow[0].binaries, ["/usr/bin/curl"]);

        // A second writer sees the first one's work rather than clobbering it.
        update_at(&path, &lock, |l| l.block("pypi.org:443")).unwrap();
        let read = Lists::load_from(&path).unwrap();
        assert_eq!(read.block.len(), 2);
        assert_eq!(read.allow.len(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    /// A file written by a future sbx that has grown a key must not stop this
    /// one from starting -- and a file missing a key it has since gained must
    /// read as an empty list rather than failing.
    #[test]
    fn a_half_written_file_still_reads() {
        let dir =
            std::env::temp_dir().join(format!("sbx-endpoints-partial-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("endpoints.json");

        fs::write(&path, r#"{"block":["pypi.org:443"]}"#).unwrap();
        let l = Lists::load_from(&path).unwrap();
        assert!(l.allow.is_empty());
        assert_eq!(l.verdict("pypi.org:443"), Some(Listed::Blocked));

        let _ = fs::remove_dir_all(&dir);
    }
}
