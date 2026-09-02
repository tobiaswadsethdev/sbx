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
import { GitView } from "./GitView";
import { Facts } from "./panes/Facts";
import { PolicyPane } from "./panes/Policy";
import { EventsPane } from "./panes/Events";

const VIEWS = ["files", "git", "events", "policy", "facts"] as const;
type View = (typeof VIEWS)[number];

export function Dock({
  server,
  session,
  onOpenFile,
  onOpenDiff,
}: {
  server: string;
  session: Session;
  onOpenFile: (path: string) => void;
  onOpenDiff: (path: string, against: Against) => void;
}) {
  // Files first: it is the one you reach for without having decided anything
  // yet.
  const [view, setView] = useState<View>("files");

  return (
    <aside className="dock">
      <nav className="dock-tabs">
        {VIEWS.map((v) => (
          <button key={v} className={v === view ? "on" : ""} onClick={() => setView(v)}>
            {v}
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
        {view === "facts" && <Facts session={session} />}
        {view === "policy" && <PolicyPane server={server} name={session.name} />}
        {view === "events" && <EventsPane server={server} name={session.name} />}
      </div>
    </aside>
  );
}
