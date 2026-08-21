//! The local session cache, and reconciling it against the gateway.
//!
//! The cache is only ever a cache. The gateway knows which sandboxes exist and
//! each sandbox carries its own metadata, so losing this file costs nothing but
//! a round trip. That is what makes a crashed TUI able to re-adopt live work.

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

    pub fn replace_all(&mut self, sessions: Vec<Session>) {
        self.sessions = sessions.into_iter().map(|s| (s.name.clone(), s)).collect();
    }
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
