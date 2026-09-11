// The working copy, as git sees it: what has changed, and what to do about it.
//
// A sidebar rather than a tab, because it is a place you work *from* -- you
// look at what changed, open one, come back, stage it. A tab you have to
// reselect after every diff would make that four clicks instead of two.
//
// **The agent is editing while this is on screen.** Every action re-reads the
// status from the server rather than adjusting the list it already had: staging
// a file the agent has since rewritten stages the rewrite, and a list that
// assumed otherwise would be quietly lying about what is about to be committed.

import { useCallback, useEffect, useState } from "react";

import { api, messageOf, type GitAnswer } from "./api";
import { useConfirm } from "./Confirm";
import type { Against } from "./gen/Against";
import type { Change } from "./gen/Change";
import type { ChangedFile } from "./gen/ChangedFile";
import { Empty, Waiting } from "./Empty";
import {
  Clean,
  Fetch,
  FileIcon,
  Minus,
  Plus,
  Publish,
  Pull,
  Push,
  Refresh,
  Revert,
} from "./icons";

/// One letter per change, which is what every git client uses and what fits
/// beside a filename.
const MARK: Record<Change, string> = {
  added: "A",
  modified: "M",
  deleted: "D",
  renamed: "R",
  untracked: "U",
  conflicted: "!",
};

export function GitView({
  server,
  name,
  onOpenDiff,
}: {
  server: string;
  name: string;
  onOpenDiff: (path: string, against: Against) => void;
}) {
  const [answer, setAnswer] = useState<GitAnswer | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  /// Discarding a file is the one thing in this pane that destroys work, so it
  /// is asked for in a dialog of the window's own rather than the webview's.
  const { ask, dialog: confirmation } = useConfirm();
  const [message, setMessage] = useState("");

  const load = useCallback(() => {
    api
      .gitStatus(server, name)
      .then(setAnswer)
      .catch((e) => setError(messageOf(e)));
  }, [server, name]);

  useEffect(load, [load]);

  const act = async (action: Parameters<typeof api.git>[2]) => {
    setBusy(true);
    setError(null);
    try {
      setAnswer(await api.git(server, name, action));
      if (action.do === "commit") setMessage("");
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  };

  if (error && !answer) return <p className="error">{error}</p>;
  if (!answer) return <Waiting />;

  const { status } = answer;
  const nothing = status.staged.length === 0 && status.unstaged.length === 0;

  return (
    <div className="git">
      {confirmation}
      <header>
        <span className="branch">{status.branch}</span>
        {status.upstream ? (
          <span className="ab" title={`against ${status.upstream}`}>
            {status.ahead > 0 && `↑${status.ahead}`}
            {status.behind > 0 && `↓${status.behind}`}
            {status.ahead === 0 && status.behind === 0 && "in sync"}
          </span>
        ) : (
          // Never pushed is not the same as in sync, and the button below says
          // so rather than looking like a no-op.
          <span className="ab none">no upstream</span>
        )}
      </header>

      {/* Four operations and four glyphs, where there used to be three words
          and an icon. The words were the shortest part of this pane and still
          worth replacing, because they were three *nouns of git* -- the one
          vocabulary in the window that every reader already knows as a picture
          from every other client they have used.

          `push` and `publish` stay two different glyphs, which is the whole
          reason this is not one button with a changing tooltip: a branch the
          remote has never heard of is a different operation from a branch that
          is behind, and the pane says so above in `no upstream`. A cloud going
          up is a first push; an arrow at a line is every one after it. */}
      <div className="git-ops">
        <button
          className="op"
          disabled={busy}
          title="fetch — bring the remote's refs down without touching the working copy"
          onClick={() => void act({ do: "fetch" })}
        >
          <Fetch aria-label="fetch" />
        </button>
        <button
          className="op"
          disabled={busy}
          title="pull"
          onClick={() => void act({ do: "pull" })}
        >
          <Pull aria-label="pull" />
        </button>
        <button
          className="op"
          disabled={busy}
          title={
            status.upstream ? "push" : "publish — this branch is not on the remote yet"
          }
          onClick={() => void act({ do: "push" })}
        >
          {status.upstream ? (
            <Push aria-label="push" />
          ) : (
            <Publish aria-label="publish" />
          )}
        </button>
        <button
          className="quiet-icon refresh"
          disabled={busy}
          onClick={load}
          title="re-read git"
        >
          <Refresh aria-label="re-read git" />
        </button>
      </div>

      {error && <p className="error">{error}</p>}
      {answer.said.trim() && <pre className="said">{answer.said.trim()}</pre>}

      <Section
        title="staged"
        entries={status.staged}
        busy={busy}
        onOpen={(p) => onOpenDiff(p, "staged")}
        action={{ icon: <Minus aria-label="unstage" />, title: "unstage", run: (path) => void act({ do: "unstage", path }) }}
      />
      <Section
        title="changes"
        entries={status.unstaged}
        busy={busy}
        onOpen={(p) => onOpenDiff(p, "worktree")}
        action={{ icon: <Plus aria-label="stage" />, title: "stage", run: (path) => void act({ do: "stage", path }) }}
        discard={(path) =>
          ask({
            title: "Throw away your changes?",
            body: (
              <>
                <code>{path}</code> goes back to what it was, and the agent may be part-way
                through writing it.
              </>
            ),
            confirm: "discard",
            onConfirm: () => void act({ do: "discard", path }),
          })
        }
      />

      {/* A tick rather than `nothing changed`, and it is the only absence in
          the window drawn in the colour that means "it passed": every other
          empty pane is neutral because empty is neither good nor bad, and a
          clean working copy is the one case where it is an answer. */}
      {nothing && <Empty icon={Clean} tone="ok" />}

      <div className="commit">
        <textarea
          rows={2}
          value={message}
          placeholder="commit message"
          onChange={(e) => setMessage(e.target.value)}
        />
        <button
          className="go"
          // Nothing staged is the one case git refuses outright, and a button
          // that produces an error you could have been shown is a bad button.
          disabled={busy || !message.trim() || status.staged.length === 0}
          onClick={() => void act({ do: "commit", message })}
        >
          commit {status.staged.length > 0 && `(${status.staged.length})`}
        </button>
      </div>
    </div>
  );
}

function Section({
  title,
  entries,
  busy,
  onOpen,
  action,
  discard,
}: {
  title: string;
  entries: ChangedFile[];
  busy: boolean;
  onOpen: (path: string) => void;
  action: { icon: React.ReactNode; title: string; run: (path: string) => void };
  discard?: (path: string) => void;
}) {
  if (entries.length === 0) return null;
  return (
    <section className="changes">
      <h3>
        {title} <span className="count">{entries.length}</span>
      </h3>
      {entries.map((e) => (
        <div className="change" key={`${title}:${e.path}`}>
          <button className="open" onClick={() => onOpen(e.path)} title={e.path}>
            <span className={`mark ${e.change}`}>{MARK[e.change]}</span>
            <FileIcon name={e.path} />
            <span className="path">{e.path}</span>
          </button>
          {discard && (
            <button className="act" disabled={busy} title="discard" onClick={() => discard(e.path)}>
              <Revert aria-label="discard" />
            </button>
          )}
          <button className="act" disabled={busy} title={action.title} onClick={() => action.run(e.path)}>
            {action.icon}
          </button>
        </div>
      ))}
    </section>
  );
}
