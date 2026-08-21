//! Operations shared by the CLI and the TUI.

use openshell_client::OpenShell;

use crate::seed;
use crate::session::{REPO_PATH, SELECTOR_MANAGED, Session};
use crate::store::{self, Store};

#[derive(Debug, Default)]
pub struct Refreshed {
    pub sessions: Vec<Session>,
    /// Sessions recovered from a sandbox the cache did not know about.
    pub adopted: Vec<String>,
    /// Sessions whose sandbox has just disappeared.
    pub dead: Vec<String>,
    /// Non-fatal problems, e.g. a sandbox that could not be adopted.
    pub warnings: Vec<String>,
}

/// Reconcile the cache against the gateway, adopt orphans, and persist.
pub fn refresh(client: &dyn OpenShell) -> Result<Refreshed, Box<dyn std::error::Error>> {
    let mut store = Store::load()?;
    let live = client.list(Some(SELECTOR_MANAGED))?;
    let rec = store::reconcile(store.list().into_iter().cloned().collect(), &live);

    let mut out = Refreshed {
        sessions: rec.sessions,
        dead: rec.dead,
        ..Default::default()
    };

    for orphan in &rec.orphans {
        let sandbox = format!("sbx-{orphan}");
        match seed::read_meta(client, &sandbox) {
            Ok(s) => {
                out.adopted.push(s.name.clone());
                out.sessions.push(s);
            }
            Err(e) => out.warnings.push(format!("could not adopt {sandbox}: {e}")),
        }
    }

    out.sessions.sort_by(|a, b| a.name.cmp(&b.name));
    store.replace_all(out.sessions.clone());
    store.save()?;
    Ok(out)
}

/// A snapshot of the repository inside a session's sandbox, for the preview
/// pane. One exec, so the pane costs a single round trip per session.
pub fn repo_preview(client: &dyn OpenShell, session: &Session) -> String {
    let script = format!(
        r#"cd {repo} 2>/dev/null || {{ echo "no repository at {repo}"; exit 0; }}
echo "branch  $(git rev-parse --abbrev-ref HEAD 2>/dev/null)"
echo "commit  $(git --no-pager log --oneline -1 2>/dev/null)"
changes=$(git status --porcelain 2>/dev/null)
if [ -z "$changes" ]; then
  echo "status  clean"
else
  echo "status  $(printf '%s\n' "$changes" | wc -l) file(s) changed"
  echo
  printf '%s\n' "$changes" | head -20
fi
"#,
        repo = seed::sh_quote(REPO_PATH),
    );

    match client.exec(&session.sandbox, &["sh", "-c", &script]) {
        Ok(out) if out.ok() => out.trimmed().to_string(),
        // A preview is decoration: surface the problem, never fail the caller.
        Ok(out) => format!("(could not read repository: {})", out.stderr.trim()),
        Err(e) => format!("(sandbox unreachable: {e})"),
    }
}
