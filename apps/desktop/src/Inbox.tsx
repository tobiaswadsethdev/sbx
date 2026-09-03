// The task inbox: what your trackers say is assigned to you, and one button to
// start work on it.
//
// Read on the server with the credentials in its store, so this window shows a
// list and never holds a token. What it adds over the tracker's own web UI is
// only this: the row knows how to become a session, with the task, the branch
// and the name already right -- and the session knows which ticket it came
// from, so publishing writes back to it.
//
// **A ticket does not know which repository it is about.** A Jira issue names a
// project and an Azure DevOps work item names an area path; neither is a clone
// URL, and guessing from a name would be wrong in exactly the cases where it
// matters. So a row carries a project chooser: the tracker says what to do and
// you say where.

import { useEffect, useState } from "react";

import { api, messageOf } from "./api";
import type { Inbox as View } from "./gen/Inbox";
import type { Project } from "./gen/Project";
import type { Task } from "./gen/Task";

export function InboxDialog({
  server,
  projects,
  currentProject,
  onClose,
  onStart,
}: {
  server: string;
  projects: Project[];
  /// The project of whatever is selected in the tree, which is the most likely
  /// answer to "where" and so the one a row opens on.
  currentProject: string | null;
  onClose: () => void;
  onStart: (project: Project, task: Task) => void;
}) {
  const [view, setView] = useState<View | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    api
      .tasks(server)
      .then((v) => live && setView(v))
      .catch((e) => live && setError(messageOf(e)));
    return () => {
      live = false;
    };
  }, [server]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const trackers = [...new Set((view?.tasks ?? []).map((t) => t.tracker))];

  return (
    <div className="scrim" onMouseDown={onClose}>
      <div className="dialog wide" onMouseDown={(e) => e.stopPropagation()}>
        <header className="dialog-head">
          <h2>Inbox</h2>
          <button className="quiet" onClick={onClose}>
            close
          </button>
        </header>

        {error && <p className="error">{error}</p>}
        {!view && !error && <p className="loading">asking the trackers…</p>}

        {/* A tracker that could not be read is said out loud rather than
            leaving its rows quietly missing -- which is invisible, and looks
            like having nothing assigned. */}
        {view?.warnings.map((w) => (
          <p key={w} className="warn">
            {w}
          </p>
        ))}

        {view && view.tasks.length === 0 && !error && (
          <p className="hint">
            {view.warnings.length > 0
              ? "Nothing readable."
              : "Nothing assigned to you, or no trackers configured — see docs/inbox.md."}
          </p>
        )}

        {trackers.map((tracker) => (
          <section key={tracker} className="integration">
            <h3>{tracker}</h3>
            {view!.tasks
              .filter((t) => t.tracker === tracker)
              .map((task) => (
                <Row
                  key={`${task.tracker}:${task.id}`}
                  task={task}
                  projects={projects}
                  currentProject={currentProject}
                  onStart={onStart}
                />
              ))}
          </section>
        ))}
      </div>
    </div>
  );
}

function Row({
  task,
  projects,
  currentProject,
  onStart,
}: {
  task: Task;
  projects: Project[];
  currentProject: string | null;
  onStart: (project: Project, task: Task) => void;
}) {
  const [where, setWhere] = useState(currentProject ?? projects[0]?.name ?? "");
  const project = projects.find((p) => p.name === where);

  return (
    <div className="row task">
      <span className="row-name">
        {/* The key, linked: reading the ticket is still a browser's job, and a
            row that could not be opened would be a worse list than the
            tracker's own. */}
        <a href={task.url} target="_blank" rel="noreferrer">
          {task.key}
        </a>
      </span>
      <span className="task-title" title={task.title}>
        {task.title}
      </span>
      {task.item_type && <span className="task-type">{task.item_type}</span>}
      <span className="task-status">{task.status}</span>
      <span className="row-actions">
        {projects.length > 1 ? (
          <select value={where} onChange={(e) => setWhere(e.target.value)}>
            {projects.map((p) => (
              <option key={p.name} value={p.name}>
                {p.name}
              </option>
            ))}
          </select>
        ) : (
          <span className="hint">{projects[0]?.name ?? "no project yet"}</span>
        )}
        <button
          className="quiet"
          disabled={!project}
          title={project ? `start a worktree in ${project.name}` : "make a project first"}
          onClick={() => project && onStart(project, task)}
        >
          start
        </button>
      </span>
    </div>
  );
}
