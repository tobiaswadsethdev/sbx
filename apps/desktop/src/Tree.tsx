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
//
// The shape of a row is Orca's, and deliberately: a card rather than a line,
// inset from the edge, with the agent's state in a fixed column to the left of
// the name and the facts about the branch on a second line under it. A line
// per session is the right thing when a session is a name; it stops being
// right once a session is a name, a state, a branch, a diff and an age, which
// on one line is five columns fighting over 240 pixels. What the card buys is
// that the state and the name are the only things at full contrast, so the list
// is scannable without being read -- see `style.css`, where every surface
// treatment on it lives.

import { useState } from "react";

import type { DiffStat } from "./gen/DiffStat";
import type { Project } from "./gen/Project";
import type { Session } from "./gen/Session";
import { Branch, Chevron, Forget, Plus, StateDot, Unsandboxed } from "./icons";

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

/// A group's key, and the identity the collapsed set is kept under. The same
/// expression the `key` prop uses, named because two places must agree on it.
function keyOf(g: Group): string {
  return g.project?.name ?? `repo:${g.hint}`;
}

export function Tree({
  groups,
  stats,
  selected,
  onSelect,
  onNewWorktree,
  onForget,
}: {
  groups: Group[];
  /// Each worktree's diff against its base, by session name, as the last poll
  /// reported it. Absent for a session that has not been polled yet, which is
  /// a different thing from a session with no changes and is why the row shows
  /// nothing rather than `+0/-0`.
  stats: Record<string, DiffStat | null>;
  selected: string | null;
  onSelect: (name: string) => void;
  onNewWorktree: (project: Project) => void;
  onForget: (project: Project) => void;
}) {
  // Which groups are shut. Held as the collapsed set rather than the open one
  // so a project that appears while the window is open -- created here, or by
  // someone else against the same server -- comes in expanded: the reason it
  // just appeared is usually that you asked for it.
  const [shut, setShut] = useState<ReadonlySet<string>>(new Set());
  const toggle = (key: string) =>
    setShut((all) => {
      const next = new Set(all);
      if (!next.delete(key)) next.add(key);
      return next;
    });

  return (
    <nav className="tree scrollbar-sleek" aria-label="projects and worktrees">
      {groups.map((g) => {
        const key = keyOf(g);
        const open = !shut.has(key);
        return (
          <section key={key} className="group">
            {/* The toggle and the actions are siblings rather than nested: a
                button inside a button is not markup a browser will honour, and
                the alternative -- a div with a click handler -- gives up the
                keyboard and the focus ring that make the reveal below safe. */}
            <header className="group-head">
              <button
                className="group-toggle"
                aria-expanded={open}
                title={g.hint}
                onClick={() => toggle(key)}
              >
                <Chevron open={open} className="group-twist" />
                <span className={`group-label${g.project ? "" : " loose"}`}>{g.label}</span>
                <span className="group-count">{g.worktrees.length}</span>
              </button>

              {g.project ? (
                // Hidden until the group is hovered or something inside it has
                // focus -- see `style.css`. A row of controls beside every
                // project is a row of controls you read past; the ones here
                // are for the moment you have decided to act on *this* project
                // and are already pointing at it.
                <span className="group-actions">
                  <button
                    className="quiet-icon"
                    title="new worktree in this project"
                    onClick={() => onNewWorktree(g.project!)}
                  >
                    <Plus aria-label="new worktree" />
                  </button>
                  <button
                    className="quiet-icon danger"
                    title="forget this project (its worktrees stay)"
                    onClick={() => onForget(g.project!)}
                  >
                    <Forget aria-label="forget project" />
                  </button>
                </span>
              ) : (
                // No `+` here on purpose: there is no project to start one in.
                // Making one from this group would guess which checkout on the
                // server the URL meant, and there may be several.
                <span
                  className="group-note"
                  title="not a project — created outside the workspace"
                >
                  external
                </span>
              )}
            </header>

            {open &&
              (g.worktrees.length === 0 ? (
                <p className="empty-group">no worktrees yet</p>
              ) : (
                g.worktrees.map((s) => (
                  <Worktree
                    key={s.name}
                    session={s}
                    stat={stats[s.name] ?? null}
                    on={s.name === selected}
                    onSelect={onSelect}
                  />
                ))
              ))}
          </section>
        );
      })}
    </nav>
  );
}

function Worktree({
  session: s,
  stat,
  on,
  onSelect,
}: {
  session: Session;
  stat: DiffStat | null;
  on: boolean;
  onSelect: (name: string) => void;
}) {
  return (
    <button
      className={`worktree${on ? " on" : ""}`}
      // `aria-current` rather than `aria-pressed`: this is which of several
      // things is being shown, not a control that is held down.
      aria-current={on ? "page" : undefined}
      onClick={() => onSelect(s.name)}
    >
      <span className="wt-state">
        <StateDot state={s.state} />
      </span>

      <span className="wt-body">
        <span className="wt-head">
          <span className="wt-name">{s.name}</span>
          {/* A worktree session runs on the server with the server's own
              rights, and the list is the first place that has to say so: a
              product whose pitch is isolation cannot have a kind of session
              that looks like every other row. Spelled out rather than reduced
              to the icon beside it, which would be a mark you have to have
              been taught. */}
          {s.backend === "worktree" && (
            <span className="wt-bare" title="no sandbox: runs on the server with its rights">
              <Unsandboxed />
              unsandboxed
            </span>
          )}
        </span>

        <span className="wt-meta">
          <Branch className="wt-branch-icon" />
          <span className="wt-branch">{s.work_branch}</span>
          {stat && <Stat stat={stat} />}
          <span className="wt-age" title="how long this worktree has existed">
            {age(s.created_at)}
          </span>
        </span>
      </span>
    </button>
  );
}

/// How far this worktree has diverged, in the three numbers the poll already
/// pays for. Rendered as spans rather than one string so the added and removed
/// counts can be coloured the way they are everywhere else in the window, and
/// suppressed entirely when all three are zero: `+0/-0` is a fact nobody needs
/// on eleven rows at once.
function Stat({ stat }: { stat: DiffStat }) {
  if (stat.added === 0 && stat.removed === 0 && stat.untracked === 0) return null;
  return (
    <span className="wt-stat" title="against the base branch">
      {stat.added > 0 && <span className="added">+{stat.added}</span>}
      {stat.removed > 0 && <span className="removed">−{stat.removed}</span>}
      {stat.untracked > 0 && (
        <span className="untracked" title={`${stat.untracked} untracked`}>
          ?{stat.untracked}
        </span>
      )}
    </span>
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
