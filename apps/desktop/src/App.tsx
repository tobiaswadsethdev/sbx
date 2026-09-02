// The window: sessions on the left, and what is true about one on the right.
//
// Deliberately shaped like the terminal interface it grew out of -- the panes
// are the same panes, because they are the part of this tool that has no
// equivalent in an ADE built on git worktrees. Policy and events are the
// isolation being visible, which is the whole pitch.
//
// Reading, and one thing that writes: `new` opens the create dialog. Everything
// else a session can be told to do still belongs to the terminal.

import { useCallback, useEffect, useState } from "react";

import { api, messageOf, type ServerSummary } from "./api";
import type { Session } from "./gen/Session";
import { Facts } from "./panes/Facts";
import { PolicyPane } from "./panes/Policy";
import { DiffPane } from "./panes/Diff";
import { EventsPane } from "./panes/Events";
import { NewSessionDialog } from "./NewSession";
import { TerminalPane } from "./panes/Terminal";
import { SessionList } from "./SessionList";

const PANES = ["terminal", "diff", "facts", "policy", "events"] as const;
export type Pane = (typeof PANES)[number];

/// How often the session list is re-read.
///
/// Slower than the terminal's second, and for a reason that is new here: every
/// refresh is a round trip to a server that may be a continent away rather than
/// an exec on this machine. The list is also the cheap request -- one call, not
/// one per session -- so this is the poll that can afford to be frequent.
const REFRESH_MS = 3000;

export default function App() {
  const [servers, setServers] = useState<ServerSummary[] | null>(null);
  const [server, setServer] = useState<string | null>(null);
  const [sessions, setSessions] = useState<Session[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [pane, setPane] = useState<Pane>("terminal");
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api
      .servers()
      .then((list) => {
        setServers(list);
        setServer((current) => current ?? list[0]?.name ?? null);
      })
      .catch((e) => setError(messageOf(e)));
  }, []);

  const refresh = useCallback(async () => {
    if (!server) return;
    try {
      const list = await api.sessions(server);
      setSessions(list);
      setError(null);
      // A session that has gone should not leave the panes showing its last
      // known state, which would be indistinguishable from it still being there.
      setSelected((current) =>
        current && list.some((s) => s.name === current) ? current : (list[0]?.name ?? null),
      );
    } catch (e) {
      setError(messageOf(e));
    }
  }, [server]);

  useEffect(() => {
    void refresh();
    const timer = setInterval(() => void refresh(), REFRESH_MS);
    return () => clearInterval(timer);
  }, [refresh]);

  const session = sessions.find((s) => s.name === selected) ?? null;

  if (servers !== null && servers.length === 0) {
    return (
      <Empty
        title="No server paired"
        body={
          <>
            <p>
              This window talks to an <code>sbxd</code>. Pair with one from a terminal:
            </p>
            <pre>
              sbxd pair desktop{"\n"}
              sbx connect 'sbx://…'
            </pre>
            <p>Then reopen this window.</p>
          </>
        }
      />
    );
  }

  return (
    <div className="app">
      <header>
        <span className="mark">sbx</span>
        {servers && servers.length > 1 ? (
          <select value={server ?? ""} onChange={(e) => setServer(e.target.value)}>
            {servers.map((s) => (
              <option key={s.name} value={s.name}>
                {s.name}
              </option>
            ))}
          </select>
        ) : (
          <span className="server">{servers?.[0]?.address ?? "…"}</span>
        )}
        <span className="count">
          {sessions.length} session{sessions.length === 1 ? "" : "s"}
        </span>
        <button className="new" disabled={!server} onClick={() => setCreating(true)}>
          new
        </button>
        {error && <span className="error">{error}</span>}
      </header>

      <main>
        <SessionList
          server={server}
          sessions={sessions}
          selected={selected}
          onSelect={setSelected}
        />

        <section className="detail">
          {session ? (
            <>
              <nav className="tabs">
                {PANES.map((p) => (
                  <button
                    key={p}
                    className={p === pane ? "on" : ""}
                    onClick={() => setPane(p)}
                  >
                    {p}
                  </button>
                ))}
              </nav>
              <div className={`pane ${pane === "terminal" ? "bare" : ""}`}>
                {pane === "terminal" && (
                  // Keyed on the session so switching sessions tears the
                  // terminal down rather than repointing a live one, which
                  // would leave the previous session's scrollback in place.
                  <TerminalPane key={session.name} server={server!} name={session.name} />
                )}
                {pane === "diff" && <DiffPane server={server!} name={session.name} />}
                {pane === "facts" && <Facts session={session} />}
                {pane === "policy" && (
                  <PolicyPane server={server!} name={session.name} />
                )}
                {pane === "events" && (
                  <EventsPane server={server!} name={session.name} />
                )}
              </div>
            </>
          ) : (
            <Empty
              title="No sessions"
              body={
                <p>
                  Start one with <b>new</b>, above, or from a terminal with{" "}
                  <code>sbx new</code>.
                </p>
              }
            />
          )}
        </section>
      </main>

      {creating && server && (
        <NewSessionDialog
          server={server}
          onClose={() => setCreating(false)}
          onCreated={(name) => {
            setCreating(false);
            // Selected before it exists, on purpose: the record is written a
            // second or two in, and the next poll finds it. Until then the
            // selection falls through to whatever is there, which is what
            // `refresh` already does for a name it cannot find.
            setSelected(name);
            void refresh();
          }}
        />
      )}
    </div>
  );
}

function Empty({ title, body }: { title: string; body: React.ReactNode }) {
  return (
    <div className="empty">
      <h1>{title}</h1>
      {body}
    </div>
  );
}
