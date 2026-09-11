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
//
// There is no count on the header's inbox glyph, and that is a decision rather
// than a gap: a badge would mean asking Jira, Azure DevOps and GitHub what is
// assigned to you on a timer, forever, for a number nobody is waiting on. The
// list is read when the screen is opened, which is when somebody has asked.

import { useEffect, useState } from "react";

import { api, messageOf } from "./api";
import { Empty, Waiting } from "./Empty";
import type { Inbox as View } from "./gen/Inbox";
import type { Project } from "./gen/Project";
import type { Task } from "./gen/Task";
import { Elsewhere, Inbox, Start } from "./icons";
import { Screen } from "./Screen";

export function InboxScreen({
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

  const trackers = [...new Set((view?.tasks ?? []).map((t) => t.tracker))];

  return (
    <Screen icon={Inbox} title="inbox" onClose={onClose}>
      {error && <p className="error">{error}</p>}
      {!view && !error && <Waiting />}

      {/* A tracker that could not be read is said out loud rather than
          leaving its rows quietly missing -- which is invisible, and looks
          like having nothing assigned. */}
      {view?.warnings.map((w) => (
        <p key={w} className="warn">
          {w}
        </p>
      ))}

      {/* An empty inbox, with the hedge kept because it is honest: `Inbox`
          carries `tasks` and `warnings` and nothing that counts configured
          trackers, so from here "you have nothing assigned" and "you have
          asked nobody" are the same answer. The note is the shortest form of
          that ambiguity rather than a guess at which half it is.

          With a warning above, there is no note at all -- the warning is the
          explanation, and repeating a maybe underneath it would be the window
          talking over itself. */}
      {view && !error && view.tasks.length === 0 && (
        <Empty
          icon={Inbox}
          note={view.warnings.length > 0 ? undefined : "nothing assigned, or no trackers yet"}
        />
      )}

      {trackers.map((tracker) => (
        <section key={tracker} className="panel">
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
    </Screen>
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
            tracker's own. The glyph after it is the window's one mark for
            "this leaves the window" -- worth the eleven pixels because the
            alternative is a link that looks exactly like the text beside it. */}
        <a href={task.url} target="_blank" rel="noreferrer" title={task.url}>
          {task.key}
          <Elsewhere />
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
        {/* The word `start` is gone from a button that appears on every row,
            and the play glyph is the one place in the window it is used -- so
            it means "begin work on this" and nothing else. The title carries
            the part that actually varies, which is *where* it will start. */}
        <button
          className="quiet-icon"
          disabled={!project}
          title={project ? `start a worktree in ${project.name}` : "make a project first"}
          onClick={() => project && onStart(project, task)}
        >
          <Start aria-label={project ? `start a worktree in ${project.name}` : "start"} />
        </button>
      </span>
    </div>
  );
}
