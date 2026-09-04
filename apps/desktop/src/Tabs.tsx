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
}) {
  return (
    <section className="editor">
      <nav className="tabs">
        {tabs.map((tab) => {
          const key = keyOf(tab);
          const shell = tab.kind === "terminal" && tab.tmux !== null ? tab.tmux : null;
          const file = tab.kind === "file" || tab.kind === "filediff" ? keyOf(tab) : null;
          return (
            <span key={key} className={`tab${key === active ? " on" : ""}`}>
              <button onClick={() => onActivate(key)} title={file ?? undefined}>
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
