//! The names of sessions that have been destroyed, kept until the thing they
//! named is really gone.
//!
//! Removing a session drops its record immediately, because the row
//! disappearing is what "remove" means to the person who asked. Deleting the
//! thing behind it is not immediate: the gateway's delete is asynchronous and a
//! sandbox stays listed for a while afterwards, and a worktree whose directory
//! could not be removed is still on disk.
//!
//! That gap is the whole bug this module exists for. A sandbox still listed,
//! labelled `sbx.session=foo`, with no record in the cache, is *exactly* the
//! shape of an orphan -- a session started by another machine, or one whose
//! cache was lost -- and [`crate::store::reconcile`] adopts orphans on purpose.
//! So the next refresh read the dead sandbox's `meta.json` and wrote the record
//! straight back, a second or two after it was removed. The session reappeared
//! in the list with its old task and its old branch, and creating a new one
//! under the same name was then refused because a session by that name existed:
//! removing a session and starting a fresh one with the same name looked like
//! sbx insisting on restarting the old one.
//!
//! A tombstone closes it. `destroy` writes the name here, adoption skips every
//! name it holds, and a refresh that could reach every backend drops the ones
//! whose sandbox or worktree has finally gone. Creating a session under the name
//! drops it too: that is someone saying out loud that the name is theirs again.
//!
//! Losing this file costs what it always cost -- a removed session may come back
//! as an orphan -- so it is written beside the session cache, in the same shape,
//! and never read as authoritative for anything else.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::session;

/// How long a tombstone may outlive the deletion it records.
///
/// A backstop rather than the mechanism: pruning happens when a refresh can see
/// that the sandbox has gone, and this is only for the machine where that never
/// happens -- a gateway taken away for good, a server reinstalled under the
/// tool. A week, because the cost of it being too long is a session nobody has
/// thought about in days not being re-adopted, and the cost of it being too
/// short is the bug above coming back on a sandbox that is genuinely stuck.
const TTL: u64 = 7 * 24 * 60 * 60;

fn default_path() -> PathBuf {
    crate::store::Store::default_path().with_file_name("removed.json")
}

/// Every name currently tombstoned.
pub fn names() -> BTreeSet<String> {
    names_at(&default_path())
}

/// Each of these has a twin taking the file, for the reason
/// [`crate::comments::list_at`] does: the tombstones live in the config
/// directory, and a test that had to move `XDG_CONFIG_HOME` to reach them would
/// race every other test doing the same.
pub fn names_at(path: &Path) -> BTreeSet<String> {
    read(path).into_keys().collect()
}

/// Record that a session has been destroyed.
pub fn remember(name: &str) {
    remember_at(&default_path(), name);
}

pub fn remember_at(path: &Path, name: &str) {
    let mut all = read(path);
    all.insert(name.to_string(), session::now_epoch());
    write(path, &all);
}

/// Drop a tombstone, because the name is in use again or the thing it named has
/// gone.
pub fn forget(name: &str) {
    forget_at(&default_path(), name);
}

pub fn forget_at(path: &Path, name: &str) {
    let mut all = read(path);
    if all.remove(name).is_some() {
        write(path, &all);
    }
}

/// Keep only the tombstones whose sandbox or worktree is still there.
///
/// Called by a refresh that reached *every* backend, and only then: a gateway
/// that could not be asked reports nothing lingering, and pruning on that would
/// forget precisely the tombstones that are still doing their job.
pub fn keep_only(still_there: &BTreeSet<String>) {
    keep_only_at(&default_path(), still_there);
}

pub fn keep_only_at(path: &Path, still_there: &BTreeSet<String>) {
    let all = read(path);
    let now = session::now_epoch();
    let kept: BTreeMap<String, u64> = all
        .iter()
        .filter(|(name, at)| still_there.contains(name.as_str()) && now.saturating_sub(**at) < TTL)
        .map(|(name, at)| (name.clone(), *at))
        .collect();
    if kept.len() != all.len() {
        write(path, &kept);
    }
}

/// A name to epoch map. Unreadable or unparseable is empty: this is a cache of
/// an absence, and failing a refresh over it would be worse than the session it
/// guards coming back.
fn read(path: &Path) -> BTreeMap<String, u64> {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Temp file and rename, like the session cache beside it.
fn write(path: &Path, all: &BTreeMap<String, u64>) {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let Ok(json) = serde_json::to_string_pretty(all) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, json).is_ok() {
        let _ = fs::rename(&tmp, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sbx-removed-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir.join("removed.json")
    }

    #[test]
    fn a_remembered_name_is_listed_and_a_forgotten_one_is_not() {
        let path = scratch("round-trip");
        assert!(names_at(&path).is_empty(), "nothing removed yet");

        remember_at(&path, "add-auth");
        remember_at(&path, "fix-nav");
        assert_eq!(
            names_at(&path),
            BTreeSet::from(["add-auth".to_string(), "fix-nav".to_string()])
        );

        forget_at(&path, "add-auth");
        assert_eq!(names_at(&path), BTreeSet::from(["fix-nav".to_string()]));

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn pruning_keeps_only_what_is_still_there() {
        let path = scratch("prune");
        remember_at(&path, "gone");
        remember_at(&path, "lingering");

        keep_only_at(&path, &BTreeSet::from(["lingering".to_string()]));

        assert_eq!(names_at(&path), BTreeSet::from(["lingering".to_string()]));

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_tombstone_older_than_the_ttl_is_dropped_even_while_it_lingers() {
        // The backstop. Written by hand rather than by waiting a week: what is
        // being asserted is that the age is read at all.
        let path = scratch("ttl");
        let stale = session::now_epoch().saturating_sub(TTL + 1);
        write(&path, &BTreeMap::from([("ancient".to_string(), stale)]));

        keep_only_at(&path, &BTreeSet::from(["ancient".to_string()]));

        assert!(names_at(&path).is_empty());

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn an_unreadable_file_is_no_tombstones_rather_than_a_failure() {
        let path = scratch("garbage");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{ not json").unwrap();

        assert!(names_at(&path).is_empty());

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
