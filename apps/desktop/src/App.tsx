// The workspace: projects on the left, what is open in the middle, and what is
// true about the worktree on the right.
//
// Shaped like an editor rather than like the terminal interface it grew out of.
// The list of sessions was the right thing when a session was the unit of work;
// it stopped being right once there were four repositories in it, because a
// flat list sorted by name says nothing about which four. So: projects contain
// worktrees, a worktree contains what you have open in it, and the dock carries
// the two things no ADE built on git worktrees has -- the policy being enforced
// and the decisions it made.

import { useCallback, useEffect, useMemo, useState } from "react";

import { api, messageOf, type Paired, type ServerSummary } from "./api";
import { ConnectDialog } from "./Connect";
import { Dock } from "./Dock";
import { InboxDialog } from "./Inbox";
import { IntegrationsDialog } from "./Integrations";
import type { Project } from "./gen/Project";
import type { Session } from "./gen/Session";
import type { Task } from "./gen/Task";
import { NewProjectDialog } from "./NewProject";
import { NewWorktreeDialog } from "./NewWorktree";
import type { Against } from "./gen/Against";
import { keyOf, Tabs, type Tab } from "./Tabs";
import { group, Tree } from "./Tree";

/// How often the worktree list is re-read. Slower than the terminal's second,
/// because every refresh is a round trip to a server that may be a continent
/// away rather than an exec on this machine.
const REFRESH_MS = 3000;

/// The tabs a worktree has open.
///
/// Derived from the sandbox rather than remembered here: what shells exist is a
/// fact about the sandbox, and one that outlives this window. A tab list kept
/// in the client would show a shell that had been closed from elsewhere and
/// hide one opened from elsewhere.
function tabsFor(shells: string[], open: Tab[]): Tab[] {
  return [
    { kind: "terminal", tmux: null, label: "agent" },
    ...shells.map((tmux): Tab => ({ kind: "terminal", tmux, label: tmux })),
    // Files and diffs last, in the order they were opened -- the one thing here
    // that is genuinely this window's state. A file is open because someone in
    // *this* window clicked it, and nothing in the sandbox knows that.
    ...open,
  ];
}


export default function App() {
  const [servers, setServers] = useState<ServerSummary[] | null>(null);
  const [server, setServer] = useState<string | null>(null);
  const [projects, setProjects] = useState<Project[]>([]);
  const [sessions, setSessions] = useState<Session[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const [creatingProject, setCreatingProject] = useState(false);
  const [creatingIn, setCreatingIn] = useState<Project | null>(null);
  const [connecting, setConnecting] = useState(false);
  const [showingIntegrations, setShowingIntegrations] = useState(false);
  const [showingInbox, setShowingInbox] = useState(false);
  /// The ticket a create was started from, carried from the inbox to the form.
  const [fromTask, setFromTask] = useState<Task | null>(null);

  /// The shells each worktree has, and which tab is in front of it. Both are
  /// per worktree: switching away and back finds it as you left it, because a
  /// shell you opened in one is not a shell in another.
  const [shells, setShells] = useState<Record<string, string[]>>({});
  const [files, setFiles] = useState<Record<string, Tab[]>>({});
  const [active, setActive] = useState<Record<string, string>>({});

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
      const [list, known] = await Promise.all([api.sessions(server), api.projects(server)]);
      setSessions(list);
      setProjects(known);
      setError(null);
      // A worktree that has gone should not leave the panes showing its last
      // known state, which is indistinguishable from it still being there.
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

  /// Pairing from the window, which used to be a terminal and a restart.
  ///
  /// The paired server is selected immediately: someone who has just pasted a
  /// pairing string is asking to look at that server, and the poll below finds
  /// its worktrees within the second.
  const paired = (p: Paired) => {
    setServers(p.servers);
    setServer(p.server.name);
    setConnecting(false);
  };

  /// Forgetting one. Selection moves off it rather than staying on a name that
  /// no longer resolves -- `remote()` on the Rust side would answer "no server
  /// named" to every request after it.
  const forgot = (list: ServerSummary[]) => {
    setServers(list);
    setServer((current) =>
      current && list.some((s) => s.name === current) ? current : (list[0]?.name ?? null),
    );
  };

  const connect = connecting ? (
    <ConnectDialog
      servers={servers ?? []}
      onClose={() => setConnecting(false)}
      onPaired={paired}
      onForgot={forgot}
    />
  ) : null;

  const groups = useMemo(() => group(projects, sessions), [projects, sessions]);
  const session = sessions.find((s) => s.name === selected) ?? null;

  // Asked once per worktree as it is selected. Not polled: a shell appears
  // because someone in this window asked for one, and paying an exec a second
  // to hear that nothing changed is what the stream exists to avoid.
  useEffect(() => {
    if (!server || !session || shells[session.name]) return;
    let live = true;
    api
      .shells(server, session.name)
      .then((list) => live && setShells((all) => ({ ...all, [session.name]: list })))
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [server, session, shells]);

  const openTabs = session ? tabsFor(shells[session.name] ?? [], files[session.name] ?? []) : [];

  /// Open a tab if it is not already open, and bring it to the front either way.
  const openTab = (worktree: string, tab: Tab) => {
    const key = keyOf(tab);
    setFiles((all) => {
      const open = all[worktree] ?? [];
      return open.some((t) => keyOf(t) === key) ? all : { ...all, [worktree]: [...open, tab] };
    });
    setActive((a) => ({ ...a, [worktree]: key }));
  };
  const activeTab = session ? (active[session.name] ?? keyOf(openTabs[0])) : "";

  if (servers !== null && servers.length === 0) {
    return (
      <>
        <Empty
          title="No server paired"
          body={
            <>
              <p>
                This window talks to an <code>sbxd</code>, which may be on this
                machine or another one. On the machine with the sandboxes:
              </p>
              <pre>
                sbxd serve{"\n"}
                sbxd pair desktop --host 127.0.0.1
              </pre>
              <p>
                {/* Without --host the string carries the machine's own hostname,
                    which on a Debian-family box resolves to 127.0.1.1 while the
                    server is bound to 127.0.0.1 -- a connection refused from a
                    server that is running perfectly well. */}
                <code>--host</code> is the address this window should dial, and
                leaving it out is the usual reason a paired server cannot be
                reached. For a server on another machine, see{" "}
                <code>docs/server.md</code>.
              </p>
              <button className="go" onClick={() => setConnecting(true)}>
                paste a pairing string
              </button>
              <p className="hint">
                Or, where there is an <code>sbx</code> on this machine,{" "}
                <code>sbx connect &apos;sbx://…&apos;</code> in a terminal. Both
                save the same connection.
              </p>
            </>
          }
        />
        {connect}
      </>
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
          {sessions.length} worktree{sessions.length === 1 ? "" : "s"} in {projects.length} project
          {projects.length === 1 ? "" : "s"}
        </span>
        <button className="new" disabled={!server} onClick={() => setShowingInbox(true)}>
          inbox
        </button>
        <button className="new" disabled={!server} onClick={() => setCreatingProject(true)}>
          new project
        </button>
        <button
          className="new"
          disabled={!server}
          onClick={() => setShowingIntegrations(true)}
        >
          integrations
        </button>
        <button className="new" onClick={() => setConnecting(true)}>
          servers
        </button>
        {error && <span className="error">{error}</span>}
      </header>

      <main>
        <Tree
          groups={groups}
          selected={selected}
          onSelect={setSelected}
          onNewWorktree={setCreatingIn}
          onForget={(p) => {
            if (!server) return;
            api
              .forgetProject(server, p.name)
              .then(setProjects)
              .catch((e) => setError(messageOf(e)));
          }}
        />

        {session && server ? (
          <>
            <Tabs
              server={server}
              name={session.name}
              tabs={openTabs}
              active={activeTab}
              onActivate={(key) => setActive((a) => ({ ...a, [session.name]: key }))}
              onNewShell={() => {
                api
                  .newShell(server, session.name)
                  .then((list) => {
                    setShells((all) => ({ ...all, [session.name]: list }));
                    // Opened in front, because asking for a shell is asking to
                    // use one.
                    const opened = list[list.length - 1];
                    if (opened) {
                      setActive((a) => ({ ...a, [session.name]: `terminal:${opened}` }));
                    }
                  })
                  .catch((e) => setError(messageOf(e)));
              }}
              onCloseFile={(key) => {
                setFiles((all) => ({
                  ...all,
                  [session.name]: (all[session.name] ?? []).filter((t) => keyOf(t) !== key),
                }));
                setActive((a) =>
                  a[session.name] === key ? { ...a, [session.name]: "terminal:agent" } : a,
                );
              }}
              onCloseShell={(tmux) => {
                api
                  .killShell(server, session.name, tmux)
                  .then((list) => {
                    setShells((all) => ({ ...all, [session.name]: list }));
                    // Whatever was in front may have just been killed; the
                    // agent is the one tab that is always there.
                    setActive((a) =>
                      a[session.name] === `terminal:${tmux}`
                        ? { ...a, [session.name]: "terminal:agent" }
                        : a,
                    );
                  })
                  .catch((e) => setError(messageOf(e)));
              }}
            />
            <Dock
              server={server}
              session={session}
              onOpenFile={(path) => openTab(session.name, { kind: "file", path })}
              onOpenDiff={(path, against: Against) =>
                openTab(session.name, { kind: "filediff", path, against })
              }
            />
          </>
        ) : (
          <Empty
            title={projects.length === 0 ? "No projects" : "No worktrees"}
            body={
              projects.length === 0 ? (
                <p>
                  A project is a repository you have decided to work on. Make one with{" "}
                  <b>new project</b>, then start a worktree in it.
                </p>
              ) : (
                <p>
                  Start one with the <b>+</b> beside a project, or from a terminal with{" "}
                  <code>sbx new</code>.
                </p>
              )
            }
          />
        )}
      </main>

      {creatingProject && server && (
        <NewProjectDialog
          server={server}
          onClose={() => setCreatingProject(false)}
          onCreated={(list) => {
            setProjects(list);
            setCreatingProject(false);
          }}
        />
      )}

      {connect}

      {showingIntegrations && server && (
        <IntegrationsDialog server={server} onClose={() => setShowingIntegrations(false)} />
      )}

      {showingInbox && server && (
        <InboxDialog
          server={server}
          projects={projects}
          currentProject={sessions.find((s) => s.name === selected)?.project ?? null}
          onClose={() => setShowingInbox(false)}
          onStart={(project, task) => {
            // Straight into the create form, pre-filled: the inbox's whole
            // point is that starting work on a ticket is one step.
            setShowingInbox(false);
            setFromTask(task);
            setCreatingIn(project);
          }}
        />
      )}

      {creatingIn && server && (
        <NewWorktreeDialog
          server={server}
          project={creatingIn}
          from={fromTask}
          onClose={() => {
            setCreatingIn(null);
            setFromTask(null);
          }}
          onCreated={(name) => {
            setCreatingIn(null);
            setFromTask(null);
            // Selected before it exists, on purpose: the record is written a
            // second or two in, and the next poll finds it.
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
