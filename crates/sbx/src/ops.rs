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

/// Line cap on a fetched diff. Diffs can be arbitrarily large and the pane is
/// scrolled in memory, so the fetch is bounded rather than the render.
const DIFF_LINE_CAP: usize = 2000;

/// Marks a section heading in the diff body. Chosen because unified diff output
/// can never produce a line starting with `#` in column zero: body lines always
/// begin with `+`, `-`, ` `, `@` or `\`, and file headers with `diff`/`index`.
pub const DIFF_SECTION: &str = "### ";
/// Marks a notice (truncation, missing base branch) in the diff body.
pub const DIFF_NOTICE: &str = "!!! ";

/// How much a session's working copy has diverged from its base branch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiffStat {
    pub added: u32,
    pub removed: u32,
    /// Untracked entries. Counted as entries rather than lines: whole untracked
    /// directories collapse to one, so the count stays bounded no matter what
    /// the agent left lying around.
    pub untracked: u32,
}

impl DiffStat {
    pub fn is_empty(&self) -> bool {
        *self == DiffStat::default()
    }

    /// Parse the `<added> <removed> <untracked>` line the stat script prints.
    fn parse(s: &str) -> Option<Self> {
        let mut it = s.split_whitespace();
        let mut next = || it.next()?.parse::<u32>().ok();
        Some(DiffStat {
            added: next()?,
            removed: next()?,
            untracked: next()?,
        })
    }
}

/// Shell that resolves the base ref to diff against, leaving it in `$base`.
///
/// `git clone` sets `refs/remotes/origin/HEAD`, so the remote's default branch
/// is recoverable even when the session did not pin one. `$base` is left empty
/// if it cannot be resolved, which callers must handle: a fresh clone of a
/// repository with an unusual remote layout has no usable base.
fn resolve_base(session: &Session) -> String {
    // A stored base branch names a local branch; the remote-tracking ref is the
    // one that still points at the base after the agent commits.
    let base = match &session.base_branch {
        Some(b) => format!("origin/{b}"),
        None => String::new(),
    };
    format!(
        r#"base={base}
if [ -z "$base" ]; then
  base=$(git symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null)
fi
if [ -n "$base" ]; then
  git rev-parse --verify --quiet "$base" >/dev/null 2>&1 || base=''
fi
"#,
        base = seed::sh_quote(&base),
    )
}

/// The diff between a session's work and the branch it started from.
///
/// One exec, because exec on a sandbox is serialised: a second concurrent call
/// waits behind the first, so each pane costs exactly one round trip.
///
/// Three sections, because none of them alone is the answer. `diff base...HEAD`
/// is committed work measured from the merge-base, so commits landing on the
/// base branch afterwards never show up as the agent's. `diff HEAD` is staged
/// and unstaged work together. Untracked files appear in neither, and a new
/// file is the most common thing an agent produces.
pub fn repo_diff(client: &dyn OpenShell, session: &Session) -> String {
    let script = format!(
        r#"cd {repo} 2>/dev/null || {{ printf 'no repository at %s\n' {repo}; exit 0; }}
{resolve_base}
emit() {{
  if [ -z "$2" ]; then return 0; fi
  printf '{section}%s\n' "$1"
  total=$(printf '%s\n' "$2" | wc -l)
  if [ "$total" -gt {cap} ]; then
    printf '%s\n' "$2" | head -n {cap}
    printf '{notice}showing {cap} of %s lines; attach to read the rest\n' "$total"
  else
    printf '%s\n' "$2"
  fi
}}

any=''
if [ -n "$base" ]; then
  committed=$(git --no-pager diff --no-color "$base...HEAD" 2>/dev/null)
  if [ -n "$committed" ]; then any=y; fi
  emit "committed, vs $base" "$committed"
else
  printf '{notice}base branch could not be resolved; committed work is not shown\n'
fi

working=$(git --no-pager diff --no-color HEAD 2>/dev/null)
if [ -n "$working" ]; then any=y; fi
emit 'uncommitted' "$working"

untracked=$(git ls-files --others --exclude-standard --directory 2>/dev/null)
if [ -n "$untracked" ]; then any=y; fi
emit 'untracked' "$untracked"

if [ -z "$any" ]; then printf 'no changes yet\n'; fi
"#,
        repo = seed::sh_quote(REPO_PATH),
        resolve_base = resolve_base(session),
        section = DIFF_SECTION,
        notice = DIFF_NOTICE,
        cap = DIFF_LINE_CAP,
    );

    match client.exec(&session.sandbox, &["sh", "-c", &script]) {
        Ok(out) if out.ok() => out.trimmed().to_string(),
        Ok(out) => format!("(could not read the diff: {})", out.stderr.trim()),
        Err(e) => format!("(sandbox unreachable: {e})"),
    }
}

/// Added/removed line counts for the list column.
///
/// A single `diff --numstat` against the merge-base tree covers committed and
/// uncommitted work at once, so this stays one cheap exec: it is run for every
/// session, not just the selected one. Untracked files are counted rather than
/// read, which keeps the cost independent of what is in them.
pub fn repo_stat(client: &dyn OpenShell, session: &Session) -> Option<DiffStat> {
    let script = format!(
        r#"cd {repo} 2>/dev/null || exit 0
{resolve_base}
mb=''
if [ -n "$base" ]; then mb=$(git merge-base "$base" HEAD 2>/dev/null); fi
if [ -z "$mb" ]; then mb=HEAD; fi
tracked=$(git --no-pager diff --numstat "$mb" 2>/dev/null |
  awk '{{a+=$1; d+=$2}} END {{printf "%d %d", a+0, d+0}}')
untracked=$(git ls-files --others --exclude-standard --directory 2>/dev/null | wc -l)
printf '%s %s\n' "$tracked" "$untracked"
"#,
        repo = seed::sh_quote(REPO_PATH),
        resolve_base = resolve_base(session),
    );

    match client.exec(&session.sandbox, &["sh", "-c", &script]) {
        // A stat is decoration on a column: an unreachable sandbox or a
        // half-seeded repository leaves the column blank rather than shouting.
        Ok(out) if out.ok() => DiffStat::parse(out.trimmed()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_stat_line() {
        assert_eq!(
            DiffStat::parse("12 3 1"),
            Some(DiffStat {
                added: 12,
                removed: 3,
                untracked: 1
            })
        );
        assert_eq!(DiffStat::parse("0 0 0"), Some(DiffStat::default()));
        // The script prints the awk result and the count with a single space,
        // but a repository with no changes at all makes awk emit "0 0" and the
        // shell add the third field, so tolerate any run of whitespace.
        assert_eq!(
            DiffStat::parse("  6   0  \n"),
            None,
            "a missing field is not a zero"
        );
        assert_eq!(DiffStat::parse(""), None);
        assert_eq!(DiffStat::parse("a b c"), None);
        assert_eq!(DiffStat::parse("-1 0 0"), None, "counts are never negative");
    }

    #[test]
    fn empty_stat_is_distinguishable_from_a_measured_one() {
        assert!(DiffStat::default().is_empty());
        assert!(
            !DiffStat {
                added: 0,
                removed: 0,
                untracked: 1
            }
            .is_empty(),
            "an untracked file is a change even with no line edits"
        );
    }

    fn session() -> Session {
        Session::new(
            "t".into(),
            "https://example.com/r.git".into(),
            "task".into(),
        )
    }

    /// The base ref has to be the *remote-tracking* branch. `origin/main` still
    /// points at the base after the agent commits to the work branch; a local
    /// `main` would be left behind by a `git switch -c`, and diffing against it
    /// would credit the agent with everything on the base branch.
    #[test]
    fn base_resolution_prefers_the_remote_tracking_ref() {
        let mut s = session();
        s.base_branch = Some("develop".into());
        let script = resolve_base(&s);
        assert!(script.contains("base='origin/develop'"), "{script}");

        // With no pinned base, the clone's origin/HEAD is the fallback.
        s.base_branch = None;
        let script = resolve_base(&s);
        assert!(script.contains("base=''"), "{script}");
        assert!(script.contains("refs/remotes/origin/HEAD"), "{script}");
    }

    #[test]
    fn base_resolution_quotes_a_hostile_branch_name() {
        let mut s = session();
        s.base_branch = Some("a'; rm -rf /; echo '".into());
        let script = resolve_base(&s);
        assert!(
            !script.contains("rm -rf /;\n") && script.contains(r"'\''"),
            "the branch name must stay inside one quoted word: {script}"
        );
    }

    /// The section and notice sigils are a contract between the fetch script
    /// and the renderer, which strips them. If they drift the pane shows raw
    /// markers.
    #[test]
    fn the_diff_script_emits_the_markers_the_renderer_strips() {
        assert_eq!(DIFF_SECTION, "### ");
        assert_eq!(DIFF_NOTICE, "!!! ");
    }
}
