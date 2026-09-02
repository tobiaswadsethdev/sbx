// What is true about the selected worktree, always on screen.
//
// Facts, policy and events do not live in the tab bar with the terminal and the
// files, and that is deliberate. The isolation being *visible* is the reason
// this is worth building rather than adopting an ADE built on git worktrees --
// and a denial you have to open a tab to find is one you will not find. It
// costs width that the editor would otherwise have; that is the trade.

import { useState } from "react";

import type { Session } from "./gen/Session";
import { Facts } from "./panes/Facts";
import { PolicyPane } from "./panes/Policy";
import { EventsPane } from "./panes/Events";

const VIEWS = ["events", "policy", "facts"] as const;
type View = (typeof VIEWS)[number];

export function Dock({ server, session }: { server: string; session: Session }) {
  // Events first: the others are what the session *is*, and this is what it has
  // been doing, which is the one that changes while you watch.
  const [view, setView] = useState<View>("events");

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
        {view === "facts" && <Facts session={session} />}
        {view === "policy" && <PolicyPane server={server} name={session.name} />}
        {view === "events" && <EventsPane server={server} name={session.name} />}
      </div>
    </aside>
  );
}
