//! The local session cache, and reconciling it against the gateway.
//!
//! The cache is only ever a cache. The gateway knows which sandboxes exist and
//! each sandbox carries its own metadata, so losing this file costs nothing but
//! a round trip. That is what makes a crashed TUI able to re-adopt live work.
//!
//! **Every change goes through [`update`], which locks the file.** More than one
//! writer is the normal case, not an edge: a TUI refreshes the whole list on a
//! timer while a `sbx new` in another terminal walks a session through
//! `creating`, `seeding`, `ready` -- and a create takes minutes on a large
//! repository. Load-modify-save without a lock loses whichever write lands
//! second, which showed up as a session whose sandbox was perfectly healthy --
//! cloned, branched, agent running -- and whose record still said `seeding`.
//! `save` alone is atomic (temp file and rename); it is the *read* before it that
//! has to be inside the same lock.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;

use openshell_client::{Phase, Sandbox};

use crate::session::{LABEL_SESSION, Session, State};

#[derive(Debug, Default)]
pub struct Store {
    path: PathBuf,
    sessions: BTreeMap<String, Session>,
}

impl Store {
    /// `$XDG_CONFIG_HOME/sbx/sessions.json`, falling back to `~/.config`.
    pub fn default_path() -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("sbx").join("sessions.json")
    }

    pub fn load() -> io::Result<Self> {
        Self::load_from(Self::default_path())
    }

    pub fn load_from(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let sessions = match fs::read_to_string(&path) {
            Ok(text) => {
                let list: Vec<Session> = serde_json::from_str(&text).map_err(io::Error::other)?;
                list.into_iter().map(|s| (s.name.clone(), s)).collect()
            }
            // A missing cache is the normal first-run case, not an error.
            Err(e) if e.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e),
        };
        Ok(Store { path, sessions })
    }

    /// Write via a temporary file and rename, so an interrupted save cannot
    /// truncate an existing cache.
    pub fn save(&self) -> io::Result<()> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let list: Vec<&Session> = self.sessions.values().collect();
        let json = serde_json::to_string_pretty(&list).map_err(io::Error::other)?;
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, json)?;
        fs::rename(&tmp, &self.path)
    }

    pub fn list(&self) -> Vec<&Session> {
        self.sessions.values().collect()
    }

    pub fn get(&self, name: &str) -> Option<&Session> {
        self.sessions.get(name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.sessions.contains_key(name)
    }

    pub fn upsert(&mut self, session: Session) {
        self.sessions.insert(session.name.clone(), session);
    }

    pub fn remove(&mut self, name: &str) -> Option<Session> {
        self.sessions.remove(name)
    }

    /// Take the state of every session listed, leaving any record not mentioned
    /// alone.
    ///
    /// Not `replace_all`, which is what this was: a refresh knows only what it
    /// loaded, and a create running in another process may have added a session
    /// since. Wholesale replacement dropped that record; merging keeps it.
    /// Removal is [`Store::remove`]'s job, which is what destroying a session
    /// calls.
    pub fn merge(&mut self, sessions: Vec<Session>) {
        for s in sessions {
            self.sessions.insert(s.name.clone(), s);
        }
    }
}

/// Where the lock lives. Beside the cache, so a stale one is obvious.
fn lock_path() -> PathBuf {
    Store::default_path().with_extension("lock")
}

/// Read the cache, change it, and write it back, with the file locked
/// throughout.
///
/// The lock is held across the read *and* the write, which is the whole point:
/// two processes each doing load-modify-save without it lose one of the two
/// changes, and the loser is whichever finished first. Held for the length of a
/// file read and a rename -- microseconds -- so nothing waits on it meaningfully.
/// Slow work (a gateway call, an exec) belongs outside.
pub fn update<T>(f: impl FnOnce(&mut Store) -> T) -> io::Result<T> {
    update_at(Store::default_path(), lock_path(), f)
}

/// [`update`], against a given pair of paths, so it can be tested.
pub fn update_at<T>(
    store: impl Into<PathBuf>,
    lock: impl Into<PathBuf>,
    f: impl FnOnce(&mut Store) -> T,
) -> io::Result<T> {
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
        let mut s = Store::load_from(store)?;
        let out = f(&mut s);
        s.save()?;
        Ok(out)
    })();

    // Explicit rather than left to the drop, so the order is visible: the write
    // above has to be inside the lock.
    let _ = guard.unlock();
    result
}

/// What reconciling the cache against live sandboxes produced.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reconciliation {
    /// Cached sessions with state corrected against the gateway.
    pub sessions: Vec<Session>,
    /// Session names present at the gateway but missing from the cache. Their
    /// metadata has to be read out of the sandbox before they can be added.
    pub orphans: Vec<String>,
    /// Sessions whose sandbox has disappeared.
    pub dead: Vec<String>,
}

/// Correct cached state against what the gateway reports.
///
/// Pure so it can be tested without a gateway. State is only changed where the
/// evidence is unambiguous: an absent sandbox, an explicitly failed one, or a
/// sandbox that has come back after being marked dead. Anything else is left
/// alone, because a create may still be in flight.
pub fn reconcile(cached: Vec<Session>, live: &[Sandbox]) -> Reconciliation {
    let by_name: BTreeMap<&str, &Sandbox> = live.iter().map(|s| (s.name.as_str(), s)).collect();

    let mut out = Reconciliation::default();

    for mut session in cached {
        match by_name.get(session.sandbox.as_str()) {
            None => {
                if session.state != State::Dead {
                    out.dead.push(session.name.clone());
                }
                session.state = State::Dead;
            }
            Some(sb) => match sb.phase {
                // Deletion is asynchronous: the sandbox stays listed as
                // `Deleting` for a while, and treating that as alive leaves a
                // removed session showing as healthy.
                Phase::Deleting => {
                    if session.state != State::Dead {
                        out.dead.push(session.name.clone());
                    }
                    session.state = State::Dead;
                }
                Phase::Error => session.state = State::Failed,
                Phase::Stopped => session.state = State::Idle,
                Phase::Ready if session.state == State::Dead => session.state = State::Ready,
                _ => {}
            },
        }
        out.sessions.push(session);
    }

    let known: Vec<&str> = out.sessions.iter().map(|s| s.name.as_str()).collect();
    for sb in live {
        // Not a sandbox on its way out. Deletion is asynchronous, so a session
        // destroyed a moment ago is still listed for a while -- and with its
        // record already dropped it looks exactly like an orphan worth adopting.
        // Reading its metadata then fails with "sandbox not found", which is a
        // frightening thing to print for a deletion that worked.
        if sb.phase == Phase::Deleting {
            continue;
        }
        if let Some(name) = sb.labels.get(LABEL_SESSION)
            && !known.contains(&name.as_str())
        {
            out.orphans.push(name.clone());
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use openshell_client::{Phase, Sandbox};

    use super::*;
    use crate::session::{LABEL_MANAGED, Session};

    fn sandbox(name: &str, phase: Phase, session_label: Option<&str>) -> Sandbox {
        let mut labels = BTreeMap::new();
        labels.insert(LABEL_MANAGED.to_string(), "true".to_string());
        if let Some(s) = session_label {
            labels.insert(LABEL_SESSION.to_string(), s.to_string());
        }
        Sandbox {
            id: format!("id-{name}"),
            name: name.to_string(),
            phase,
            created_at: "2026-08-21 14:15:56".to_string(),
            labels,
            workspace: "default".to_string(),
        }
    }

    fn session(name: &str, state: State) -> Session {
        let mut s = Session::new(name.into(), "repo".into(), "task".into());
        s.state = state;
        s
    }

    /// The bug this lock exists for, as the property that prevents it: a writer
    /// always reads the state that is on disk *now*, never one it loaded earlier.
    ///
    /// What went wrong without it: a create held a snapshot from before its clone
    /// -- minutes, on a large repository -- and wrote `ready` into it, while a TUI
    /// refreshing every second wrote the list back from its own older read. The
    /// last write won and it was usually the refresh, so a session whose sandbox
    /// was cloned, branched and running an agent had a record still saying
    /// `seeding`.
    #[test]
    fn every_writer_reads_the_state_that_is_there_now() {
        let dir = TempDir::new("store-race");
        let path = dir.0.join("sessions.json");
        let lock = dir.0.join("sessions.lock");

        update_at(&path, &lock, |s| s.upsert(session("a", State::Seeding))).unwrap();

        // Anyone holding a snapshot from before this write is holding `seeding`.
        let stale = Store::load_from(&path).unwrap();
        assert_eq!(stale.get("a").map(|s| s.state), Some(State::Seeding));

        // The create finishes.
        update_at(&path, &lock, |s| s.upsert(session("a", State::Ready))).unwrap();

        // Every later writer -- including the one that had the stale copy, since
        // its write also goes through here -- sees `ready` and not what it read.
        let seen = update_at(&path, &lock, |s| s.get("a").map(|x| x.state)).unwrap();
        assert_eq!(seen, Some(State::Ready));
    }

    /// A record added by another process during a refresh must not be dropped by
    /// it. `replace_all` dropped exactly this.
    #[test]
    fn a_refresh_leaves_records_it_never_saw_alone() {
        let dir = TempDir::new("store-merge");
        let path = dir.0.join("sessions.json");
        let lock = dir.0.join("sessions.lock");

        update_at(&path, &lock, |s| s.upsert(session("old", State::Ready))).unwrap();
        // A create in another process adds one.
        update_at(&path, &lock, |s| s.upsert(session("new", State::Seeding))).unwrap();
        // The refresh writes back only what it knew about.
        update_at(&path, &lock, |s| s.merge(vec![session("old", State::Idle)])).unwrap();

        let after = Store::load_from(&path).unwrap();
        assert_eq!(after.get("old").map(|s| s.state), Some(State::Idle));
        assert!(after.contains("new"), "the record the refresh never saw");
    }

    /// Two writers at once, for real: the lock has to serialise them, and both
    /// changes have to be there afterwards.
    #[test]
    fn concurrent_writers_both_land() {
        let dir = TempDir::new("store-threads");
        let path = dir.0.join("sessions.json");
        let lock = dir.0.join("sessions.lock");

        let threads: Vec<_> = (0..8)
            .map(|i| {
                let (path, lock) = (path.clone(), lock.clone());
                std::thread::spawn(move || {
                    update_at(&path, &lock, |s| {
                        // A read, a pause, then a write -- the shape that loses a
                        // change without a lock around both halves.
                        let seen = s.list().len();
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        s.upsert(session(&format!("s{i}"), State::Ready));
                        seen
                    })
                    .unwrap()
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }

        let after = Store::load_from(&path).unwrap();
        assert_eq!(after.list().len(), 8, "every writer's session is there");
    }

    /// `sbx rm` followed by `sbx ls` used to print "could not adopt ...: sandbox
    /// not found": the record was gone and the sandbox was still listed as
    /// `Deleting`, which together look like an orphan. A deletion that worked
    /// must not report an error.
    #[test]
    fn a_sandbox_being_deleted_is_not_an_orphan() {
        let live = vec![sandbox("sbx-gone", Phase::Deleting, Some("gone"))];
        let out = reconcile(vec![], &live);
        assert!(out.orphans.is_empty(), "{:?}", out.orphans);
    }

    #[test]
    fn missing_sandbox_marks_session_dead() {
        let r = reconcile(vec![session("a", State::Ready)], &[]);
        assert_eq!(r.sessions[0].state, State::Dead);
        assert_eq!(r.dead, vec!["a"]);
    }

    #[test]
    fn already_dead_session_is_not_reported_twice() {
        let r = reconcile(vec![session("a", State::Dead)], &[]);
        assert_eq!(r.sessions[0].state, State::Dead);
        assert!(r.dead.is_empty(), "should only report the transition");
    }

    #[test]
    fn deleting_phase_counts_as_dead() {
        // Regression: a deleted sandbox lingers in `Deleting` and used to keep
        // reporting the last cached state, so `ls` showed it as ready.
        let live = [sandbox("sbx-a", Phase::Deleting, Some("a"))];
        let r = reconcile(vec![session("a", State::Ready)], &live);
        assert_eq!(r.sessions[0].state, State::Dead);
        assert_eq!(r.dead, vec!["a"]);
    }

    #[test]
    fn stopped_sandbox_reads_as_idle() {
        let live = [sandbox("sbx-a", Phase::Stopped, Some("a"))];
        let r = reconcile(vec![session("a", State::Ready)], &live);
        assert_eq!(r.sessions[0].state, State::Idle);
    }

    #[test]
    fn error_phase_overrides_cached_state() {
        let live = [sandbox("sbx-a", Phase::Error, Some("a"))];
        let r = reconcile(vec![session("a", State::Ready)], &live);
        assert_eq!(r.sessions[0].state, State::Failed);
        assert!(r.dead.is_empty());
    }

    #[test]
    fn returning_sandbox_revives_a_dead_session() {
        let live = [sandbox("sbx-a", Phase::Ready, Some("a"))];
        let r = reconcile(vec![session("a", State::Dead)], &live);
        assert_eq!(r.sessions[0].state, State::Ready);
    }

    #[test]
    fn in_flight_states_are_left_alone() {
        // A create still running must not be clobbered into Ready.
        let live = [sandbox("sbx-a", Phase::Provisioning, Some("a"))];
        let r = reconcile(vec![session("a", State::Seeding)], &live);
        assert_eq!(r.sessions[0].state, State::Seeding);
    }

    #[test]
    fn unknown_managed_sandbox_is_an_orphan() {
        let live = [
            sandbox("sbx-a", Phase::Ready, Some("a")),
            sandbox("sbx-b", Phase::Ready, Some("b")),
        ];
        let r = reconcile(vec![session("a", State::Ready)], &live);
        assert_eq!(
            r.orphans,
            vec!["b"],
            "b exists at the gateway but not in cache"
        );
    }

    #[test]
    fn managed_sandbox_without_a_session_label_is_ignored() {
        let live = [sandbox("sbx-weird", Phase::Ready, None)];
        let r = reconcile(vec![], &live);
        assert!(r.orphans.is_empty());
    }

    /// A directory of its own per test, removed on drop so a failing assertion
    /// does not leave one behind.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "sbx-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn store_roundtrips_through_disk() {
        let dir = std::env::temp_dir().join(format!("sbx-test-{}", std::process::id()));
        let path = dir.join("sessions.json");
        let _ = std::fs::remove_dir_all(&dir);

        let mut store = Store::load_from(&path).unwrap();
        assert!(store.list().is_empty(), "missing file must load as empty");

        store.upsert(session("a", State::Ready));
        store.save().unwrap();

        let reloaded = Store::load_from(&path).unwrap();
        assert_eq!(reloaded.list().len(), 1);
        assert_eq!(reloaded.get("a").unwrap().state, State::Ready);
        assert!(reloaded.contains("a"));

        // The temporary file must not be left behind.
        assert!(!path.with_extension("json.tmp").exists());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
