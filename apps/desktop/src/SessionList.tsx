// The list, and the state column that is the reason to look at it.
//
// The state a session reports is the *sandbox's*, and for a running one the
// agent's own is more interesting -- waiting on a permission prompt above all,
// which is the thing you would otherwise discover by attaching. That costs one
// request per ready session, so it is polled separately and more slowly than
// the list itself, and a session whose poll has not come back yet shows what
// the record says rather than nothing.

import { useEffect, useState } from "react";

import { api } from "./api";
import type { Session } from "./gen/Session";
import type { State } from "./gen/State";

/// Slower than the list, because it is one request per ready session rather
/// than one for all of them. The terminal interface round-robins for the same
/// reason; this will too when a list gets long enough to need it.
const AGENT_POLL_MS = 5000;

export function SessionList({
  server,
  sessions,
  selected,
  onSelect,
}: {
  server: string | null;
  sessions: Session[];
  selected: string | null;
  onSelect: (name: string) => void;
}) {
  const agent = useAgentStates(server, sessions);

  return (
    <nav className="sessions">
      {sessions.map((s) => {
        const state = agent[s.name] ?? s.state;
        return (
          <button
            key={s.name}
            className={`session ${s.name === selected ? "on" : ""}`}
            onClick={() => onSelect(s.name)}
          >
            <span className="name">{s.name}</span>
            <StateBadge state={state} />
            <span className="branch">{s.work_branch}</span>
            <span className="age">{age(s.created_at)}</span>
          </button>
        );
      })}
    </nav>
  );
}

function useAgentStates(server: string | null, sessions: Session[]) {
  const [states, setStates] = useState<Record<string, State>>({});
  // The names, as a stable string: `sessions` is a fresh array every refresh,
  // and depending on it directly would restart the poll every three seconds.
  const ready = sessions
    .filter((s) => s.state === "ready")
    .map((s) => s.name)
    .join(",");

  useEffect(() => {
    if (!server || !ready) return;
    let live = true;

    const tick = async () => {
      for (const name of ready.split(",")) {
        try {
          const poll = await api.poll(server, name);
          if (!live) return;
          if (poll.status) {
            setStates((prev) => ({ ...prev, [name]: poll.status!.state }));
          }
        } catch {
          // One session that cannot be polled should not stop the others, and
          // the row keeps the state its record last reported.
        }
      }
    };

    void tick();
    const timer = setInterval(() => void tick(), AGENT_POLL_MS);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [server, ready]);

  return states;
}

export function StateBadge({ state }: { state: State }) {
  return <span className={`state ${state}`}>{state}</span>;
}

/// Relative, not absolute: the question a list answers is "how long has this
/// been going", and the record stores epoch seconds precisely so the display
/// can choose.
export function age(createdAt: number): string {
  const secs = Math.max(0, Math.floor(Date.now() / 1000) - createdAt);
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h`;
  return `${Math.floor(secs / 86400)}d`;
}
