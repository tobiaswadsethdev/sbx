// The editor area: one tab bar per worktree, and whatever is open in it.
//
// Tabs are per worktree rather than global. A tab is a thing you have open *in*
// a working copy -- its terminal, its diff, and soon a file in it -- so
// switching worktree switches the set, and coming back finds it as you left it.
//
// The shape allows more than it currently opens: a `file` tab is in the type
// because the file tree is the next increment, and a second `terminal` because
// extra shells beside the agent are the one after. Both are held here rather
// than added later so the tab bar is not rebuilt around them twice.

import { useState } from "react";

import { FilePane } from "./panes/File";
import { FileDiffPane } from "./panes/FileDiff";
import type { Against } from "./gen/Against";
import { Close, Plus } from "./icons";
import { TerminalPane } from "./panes/Terminal";

export type Tab =
  /// A terminal in the sandbox. `tmux` names which session, and `null` is the
  /// agent's own -- so a shell is a second tab rather than a second pane, and
  /// nothing has to remember which one is special.
  | { kind: "terminal"; tmux: string | null; label: string }
  | { kind: "file"; path: string }
  /// One file's diff, side by side. `against` is part of the key: the staged
  /// and unstaged diffs of one file are two different questions, and opening
  /// one should not replace the other.
  | { kind: "filediff"; path: string; against: Against };

export function keyOf(tab: Tab): string {
  switch (tab.kind) {
    case "terminal":
      return `terminal:${tab.tmux ?? "agent"}`;
    case "file":
      return `file:${tab.path}`;
    case "filediff":
      return `filediff:${tab.against}:${tab.path}`;
  }
}

export function labelOf(tab: Tab): string {
  switch (tab.kind) {
    case "terminal":
      return tab.label;
    case "file":
      return tab.path.split("/").pop() ?? tab.path;
    case "filediff":
      return `${tab.path.split("/").pop() ?? tab.path} ~`;
  }
}

export function Tabs({
  server,
  name,
  tabs,
  active,
  onActivate,
  onNewShell,
  onCloseShell,
  onCloseFile,
  onReorder,
}: {
  server: string;
  name: string;
  tabs: Tab[];
  active: string;
  onActivate: (key: string) => void;
  onNewShell: () => void;
  /// Closing a shell kills what is running in it, which is why only a shell has
  /// the button: the agent's terminal is not yours to close, and the diff is
  /// not a thing that can be.
  onCloseShell: (tmux: string) => void;
  onCloseFile: (path: string) => void;
  /// Put `moved` where `onto` currently is. The order lives with the caller
  /// because the tab list is derived -- see `tabsFor` -- so this says what the
  /// user did and lets the caller decide what that means for a list it rebuilds
  /// from the sandbox on every poll.
  onReorder: (moved: string, onto: string) => void;
}) {
  /// The tab being dragged, and the one it is currently over. Local because
  /// nothing outside the bar can see a drag in progress: it ends in an
  /// `onReorder` or in nothing at all.
  const [dragging, setDragging] = useState<string | null>(null);
  const [over, setOver] = useState<string | null>(null);

  const endDrag = () => {
    setDragging(null);
    setOver(null);
  };

  return (
    <section className="editor">
      <nav className="tabs">
        {tabs.map((tab) => {
          const key = keyOf(tab);
          const shell = tab.kind === "terminal" && tab.tmux !== null ? tab.tmux : null;
          const file = tab.kind === "file" || tab.kind === "filediff" ? keyOf(tab) : null;
          /// Middle click closes exactly what the cross closes, and nothing
          /// else: the agent's terminal has no cross because it is not yours to
          /// close, and a middle click that killed it would be the same mistake
          /// made faster.
          const close = shell ? () => onCloseShell(shell) : file ? () => onCloseFile(file) : null;
          return (
            <span
              key={key}
              className={`tab${key === active ? " on" : ""}${dragging === key ? " dragging" : ""}${
                over === key && dragging !== key ? " over" : ""
              }`}
              draggable
              onDragStart={(e) => {
                setDragging(key);
                // Move rather than copy, so the cursor says what will happen,
                // and some text on the clipboard because Firefox will not start
                // a drag without it.
                e.dataTransfer.effectAllowed = "move";
                e.dataTransfer.setData("text/plain", key);
              }}
              onDragOver={(e) => {
                // Both needed: without the default prevented the drop never
                // fires, and this is also the only event that reports where the
                // pointer now is.
                e.preventDefault();
                e.dataTransfer.dropEffect = "move";
                setOver(key);
              }}
              onDrop={(e) => {
                e.preventDefault();
                if (dragging && dragging !== key) onReorder(dragging, key);
                endDrag();
              }}
              // Fires whether the drag ended in a drop or was abandoned, which
              // is what makes it the one place the highlight has to come off.
              onDragEnd={endDrag}
              onAuxClick={(e) => {
                if (e.button !== 1 || !close) return;
                // Middle click pastes the selection on X11 and autoscrolls on
                // Windows; neither is wanted on a tab.
                e.preventDefault();
                close();
              }}
            >
              {/* Draggable as well as the pill around it: a button does not
                  always start a drag of its draggable ancestor -- WebKit is the
                  one that matters here -- and the label is most of the tab, so
                  it is where a drag will actually be started from. The event
                  bubbles to the handler above either way. */}
              <button draggable onClick={() => onActivate(key)} title={file ?? undefined}>
                {labelOf(tab)}
              </button>
              {shell && (
                <button
                  className="close"
                  title="close this shell, and whatever is running in it"
                  onClick={() => onCloseShell(shell)}
                >
                  <Close aria-label="close" />
                </button>
              )}
              {file && (
                <button className="close" title="close" onClick={() => onCloseFile(file)}>
                  <Close aria-label="close" />
                </button>
              )}
            </span>
          );
        })}
        <button className="add" title="another shell in this sandbox" onClick={onNewShell}>
          <Plus aria-label="new shell" />
        </button>
      </nav>

      {tabs.map((tab) => {
        const key = keyOf(tab);
        // Rendered and hidden rather than unmounted: a terminal that is
        // unmounted closes its channel and detaches, so switching to the diff
        // and back would lose the screen and re-attach. Only the terminal
        // actually needs this, but treating every tab the same means the next
        // kind added cannot get it wrong.
        return (
          <div key={key} className="tab-body" hidden={key !== active}>
            {tab.kind === "terminal" && (
              <TerminalPane
                key={`${name}:${tab.tmux ?? "agent"}`}
                server={server}
                name={name}
                tmux={tab.tmux}
              />
            )}
            {tab.kind === "filediff" && (
              <FileDiffPane
                key={keyOf(tab)}
                server={server}
                name={name}
                path={tab.path}
                against={tab.against}
              />
            )}
            {tab.kind === "file" && (
              <FilePane key={`${name}:${tab.path}`} server={server} name={name} path={tab.path} />
            )}
          </div>
        );
      })}
    </section>
  );
}
