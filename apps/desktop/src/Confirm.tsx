// Asking before something irreversible, in this window rather than the
// browser's.
//
// `window.confirm` is a webview's own dialog: it is drawn by the platform, in
// the platform's font, saying the name of the application above a line of text
// with no room in it -- and it blocks the whole webview while it is up, which
// in a window that is streaming four agents' terminals means four frozen panes.
// It also cannot say anything in the shape the rest of the window says things:
// no `code`, no danger colour on the button that does the dangerous thing, and
// no way to tell "destroy" from "discard" beyond the sentence.
//
// So it is a dialog like the others here, with the same scrim, the same escape
// key and the same buttons. Cancel is the default and gets the focus: the
// keyboard's easiest answer should be the harmless one.

import { useEffect, useRef, useState } from "react";

/// One question, and what to do when the answer is yes.
export type Ask = {
  title: string;
  /// Rendered, not interpolated: the callers say a path or a session name in
  /// `code`, which is the difference between reading the name and parsing the
  /// sentence for it.
  body: React.ReactNode;
  /// The verb on the button that goes ahead. "yes" answers nothing -- a button
  /// that says `destroy` is the last chance to notice which question this is.
  confirm: string;
  onConfirm: () => void;
};

/// The asking half of a dialog and the dialog itself.
///
/// A hook rather than a component with a boolean, because every caller has the
/// same three pieces of state -- whether it is open, what it says, what it
/// does -- and a hook is the only way not to write them three times.
export function useConfirm(): { ask: (ask: Ask) => void; dialog: React.ReactNode } {
  const [pending, setPending] = useState<Ask | null>(null);
  return {
    // Wrapped rather than handing out `setPending`, which would read a function
    // argument as a state updater.
    ask: (ask: Ask) => setPending(ask),
    dialog: pending ? (
      <ConfirmDialog ask={pending} onClose={() => setPending(null)} /> ) : null,
  };
}

function ConfirmDialog({ ask, onClose }: { ask: Ask; onClose: () => void }) {
  const cancel = useRef<HTMLButtonElement>(null);

  // The harmless button, focused: Enter and Escape then both mean "no", and
  // going ahead takes a deliberate click or a Tab first.
  useEffect(() => cancel.current?.focus(), []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div className="scrim" onMouseDown={onClose}>
      <div
        className="dialog narrow"
        role="alertdialog"
        aria-modal="true"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <header className="dialog-head">
          <h2>{ask.title}</h2>
        </header>
        <p className="confirm-body">{ask.body}</p>
        <div className="confirm-actions">
          <button ref={cancel} className="quiet" onClick={onClose}>
            cancel
          </button>
          <button
            className="go danger"
            onClick={() => {
              onClose();
              ask.onConfirm();
            }}
          >
            {ask.confirm}
          </button>
        </div>
      </div>
    </div>
  );
}
