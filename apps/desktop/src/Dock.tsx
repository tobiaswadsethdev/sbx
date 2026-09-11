// The right-hand sidebar: everything about the selected worktree that is not a
// tab.
//
// Five views over one strip. Files and git are where you work *from* -- look at
// what changed, open it, come back -- and facts, policy and events are what is
// true about the worktree. They share a sidebar rather than a tab bar in the
// middle because the middle is for what you have open: a diff you are reading
// should not have to give up its place so you can see what else changed.
//
// Policy and events stay one click away and never behind a tab in the editor,
// which is the point: the isolation being visible is why this is worth building
// rather than adopting an ADE built on git worktrees, and a denial you have to
// go looking for is one you will not find.

import { useState } from "react";

import { FileTree } from "./FileTree";
import type { Against } from "./gen/Against";
import type { Session } from "./gen/Session";
import type { Usage } from "./gen/Usage";
import { GitView } from "./GitView";
import { Branch, Events, Files, Policy, Record } from "./icons";
import { Facts } from "./panes/Facts";
import { PolicyPane } from "./panes/Policy";
import { EventsPane } from "./panes/Events";

/// The five panes, in the order the strip shows them.
///
/// A list of objects rather than a list of strings, because the strip no longer
/// renders the string. The label is still here and still doing two jobs -- the
/// tooltip and the accessible name -- it just is not drawn: five words across a
/// sidebar that may be 280 pixels wide were five words competing with the
/// filenames underneath them, and this is a strip you press once and then read
/// past for an hour.
///
/// The glyphs are the panes' subjects and not five variations on a document:
/// a tree, a branch, a pulse, a shield, an `i`. Two of them matter more than
/// the others and are worth naming -- `Policy` is a shield because that is what
/// the rules are, and `Events` is a pulse because the feed is the *evidence*
/// that the shield is doing anything. Those two panes are the reason this
/// product exists rather than an ADE built on git worktrees, and a pane nobody
/// can find is a pane nobody reads.
const VIEWS = [
  { key: "files", label: "files", icon: Files },
  { key: "git", label: "git", icon: Branch },
  { key: "events", label: "events", icon: Events },
  { key: "policy", label: "policy", icon: Policy },
  { key: "facts", label: "facts", icon: Record },
] as const;
type View = (typeof VIEWS)[number]["key"];

export function Dock({
  width,
  server,
  session,
  usage,
  onOpenFile,
  onOpenDiff,
}: {
  /// How wide, in pixels. From the window's own preferences and written by the
  /// handle on this sidebar's left edge -- see `prefs.ts` and `Split.tsx`.
  width: number;
  server: string;
  session: Session;
  /// What this session has spent, from the status channel. `null` until its
  /// agent's status line has run once.
  usage: Usage | null;
  onOpenFile: (path: string) => void;
  onOpenDiff: (path: string, against: Against) => void;
}) {
  // Files first: it is the one you reach for without having decided anything
  // yet.
  const [view, setView] = useState<View>("files");

  return (
    <aside className="dock" style={{ width }}>
      <nav className="dock-tabs" aria-label="what is true about this worktree">
        {VIEWS.map(({ key, label, icon: Mark }) => (
          <button
            key={key}
            className={key === view ? "on" : ""}
            // Which of five is showing, not a control held down -- the same
            // distinction the header's destinations make.
            aria-current={key === view ? "page" : undefined}
            title={label}
            aria-label={label}
            onClick={() => setView(key)}
          >
            <Mark />
          </button>
        ))}
      </nav>
      <div className="dock-body">
        {/* Keyed on the worktree so switching tears each down rather than
            repointing it: a file tree half-expanded into one worktree is not a
            file tree into another. */}
        {view === "files" && (
          <FileTree key={session.name} server={server} name={session.name} onOpen={onOpenFile} />
        )}
        {view === "git" && (
          <GitView
            key={session.name}
            server={server}
            name={session.name}
            onOpenDiff={onOpenDiff}
          />
        )}
        {view === "facts" && <Facts session={session} usage={usage} />}
        {view === "policy" && <PolicyPane server={server} name={session.name} />}
        {view === "events" && <EventsPane server={server} name={session.name} />}
      </div>
    </aside>
  );
}
