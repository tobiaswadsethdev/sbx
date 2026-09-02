// Projects, and the worktrees in them.
//
// A project is a repository someone has decided to work on; a worktree is one
// sandbox with one branch and one agent in it. The tree is that sentence as a
// shape, which is the whole reason it replaced a flat list: sessions across
// four repositories sorted by name told you nothing about which four
// repositories they were.
//
// Worktrees whose session records no project -- everything created from the
// terminal, which has none -- are grouped by their clone URL at the bottom
// rather than hidden or forced into one. `sbx new` is not going away, and a
// worktree started from it should still be reachable here.

import type { Project } from "./gen/Project";
import type { Session } from "./gen/Session";

export type Group = {
  /// The project, or `null` for the by-repository groups at the bottom.
  project: Project | null;
  label: string;
  hint: string;
  worktrees: Session[];
};

/// Sort sessions into their projects.
export function group(projects: Project[], sessions: Session[]): Group[] {
  const groups: Group[] = projects.map((project) => ({
    project,
    label: project.name,
    hint: project.repo,
    worktrees: sessions.filter((s) => s.project === project.name),
  }));

  // Anything whose project is gone counts as unassigned too, not just anything
  // that never had one: forgetting a project leaves its worktrees alive on
  // purpose, and they would otherwise vanish from the tree with it.
  const known = new Set(projects.map((p) => p.name));
  const loose = sessions.filter((s) => !s.project || !known.has(s.project));
  for (const repo of [...new Set(loose.map((s) => s.repo))].sort()) {
    groups.push({
      project: null,
      label: shortRepo(repo),
      hint: repo,
      worktrees: loose.filter((s) => s.repo === repo),
    });
  }
  return groups;
}

/// `https://github.com/o/thing.git` -> `o/thing`. Enough to tell two apart
/// without a line of URL per group.
function shortRepo(repo: string): string {
  const trimmed = repo.replace(/\.git$/, "").replace(/\/+$/, "");
  const parts = trimmed.split("/").filter(Boolean);
  return parts.slice(-2).join("/") || repo;
}

export function Tree({
  groups,
  selected,
  onSelect,
  onNewWorktree,
  onForget,
}: {
  groups: Group[];
  selected: string | null;
  onSelect: (name: string) => void;
  onNewWorktree: (project: Project) => void;
  onForget: (project: Project) => void;
}) {
  return (
    <nav className="tree">
      {groups.map((g) => (
        <section key={g.project?.name ?? `repo:${g.hint}`} className="group">
          <header title={g.hint}>
            <span className={`label${g.project ? "" : " loose"}`}>{g.label}</span>
            {g.project ? (
              <>
                <button
                  className="add"
                  title="new worktree in this project"
                  onClick={() => onNewWorktree(g.project!)}
                >
                  +
                </button>
                <button
                  className="add"
                  title="forget this project (its worktrees stay)"
                  onClick={() => onForget(g.project!)}
                >
                  ×
                </button>
              </>
            ) : (
              // No `+` here on purpose: there is no project to start one in.
              // Making one from this group would guess which checkout on the
              // server the URL meant, and there may be several.
              <span className="add none" title="not a project — created outside the workspace">
                ·
              </span>
            )}
          </header>

          {g.worktrees.length === 0 ? (
            <p className="empty-group">no worktrees yet</p>
          ) : (
            g.worktrees.map((s) => (
              <button
                key={s.name}
                className={`worktree${s.name === selected ? " on" : ""}`}
                onClick={() => onSelect(s.name)}
              >
                <span className="name">{s.name}</span>
                <span className={`state ${s.state}`}>{s.state}</span>
                <span className="branch">{s.work_branch}</span>
                <span className="age">{age(s.created_at)}</span>
              </button>
            ))
          )}
        </section>
      ))}
    </nav>
  );
}

/// Relative, not absolute: the question the tree answers is "how long has this
/// been going", and the record stores epoch seconds precisely so the display
/// can choose.
function age(createdAt: number): string {
  const secs = Math.max(0, Math.floor(Date.now() / 1000) - createdAt);
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h`;
  return `${Math.floor(secs / 86400)}d`;
}
