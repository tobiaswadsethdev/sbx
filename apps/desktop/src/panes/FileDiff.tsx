// One file's diff, side by side.
//
// Monaco's diff editor rather than the unified text the TUI draws, because this
// is the window and it can afford two columns. The comments are the same
// comments -- `sbx_core::comments` stores {file, line, excerpt}, which is
// already per file, so nothing about the review had to change to move it here.
//
// Monaco computes the diff in a **web worker**. Without one it renders two
// panes with no red or green in them, which reads as an empty diff rather than
// a missing worker; see panes/File.tsx, which configures it, and
// docs/desktop.md for how that was found.

import { useCallback, useEffect, useRef, useState } from "react";
import * as monaco from "monaco-editor/editor/editor.api";

import { api, messageOf } from "../api";
import type { Against } from "../gen/Against";
import type { Comment } from "../gen/Comment";
import type { FileDiff as Sides } from "../gen/FileDiff";
import { languageOf, THEME } from "./File";

export function FileDiffPane({
  server,
  name,
  path,
  against,
}: {
  server: string;
  name: string;
  path: string;
  against: Against;
}) {
  const host = useRef<HTMLDivElement>(null);
  const [sides, setSides] = useState<Sides | null>(null);
  const [review, setReview] = useState<Comment[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [editor, setEditor] = useState<monaco.editor.IStandaloneDiffEditor | null>(null);
  const [writing, setWriting] = useState<{ line: number; excerpt: string } | null>(null);
  const [draft, setDraft] = useState("");

  const load = useCallback(() => {
    Promise.all([api.gitDiff(server, name, path, against), api.comments(server, name)])
      .then(([d, c]) => {
        setSides(d);
        setReview(c);
      })
      .catch((e) => setError(messageOf(e)));
  }, [server, name, path, against]);

  useEffect(load, [load]);

  useEffect(() => {
    const element = host.current;
    if (!element || !sides || sides.binary) return;

    const created = monaco.editor.createDiffEditor(element, {
      theme: THEME,
      readOnly: true,
      domReadOnly: true,
      automaticLayout: true,
      renderSideBySide: true,
      minimap: { enabled: false },
      scrollBeyondLastLine: false,
      fontSize: 13,
    });
    const original = monaco.editor.createModel(sides.original, languageOf(path));
    const modified = monaco.editor.createModel(sides.modified, languageOf(path));
    created.setModel({ original, modified });

    // A comment is written against the *modified* side: the line you are
    // objecting to is the one that is there now, and it is the one the stored
    // line number means.
    const right = created.getModifiedEditor();
    const clicked = right.onMouseDown((e) => {
      const line = e.target.position?.lineNumber;
      if (!line) return;
      setWriting({ line, excerpt: modified.getLineContent(line) });
      setDraft("");
    });

    setEditor(created);
    return () => {
      clicked.dispose();
      original.dispose();
      modified.dispose();
      created.dispose();
      setEditor(null);
    };
  }, [sides, path]);

  // The comments that exist for this file, drawn in the margin.
  //
  // Against *this* editor, held in state rather than fished out of
  // `monaco.editor.getDiffEditors()`: two diff tabs are the normal case, and
  // "the last one created" is whichever was opened most recently, not this one.
  useEffect(() => {
    if (!editor) return;
    const mine = review.filter((c) => c.file === path);
    if (mine.length === 0) return;
    const collection = editor.getModifiedEditor().createDecorationsCollection(
      mine.map((c) => ({
        range: new monaco.Range(Math.max(1, c.line), 1, Math.max(1, c.line), 1),
        options: {
          isWholeLine: true,
          className: "commented-line",
          glyphMarginClassName: "commented-glyph",
          glyphMarginHoverMessage: { value: c.body },
        },
      })),
    );
    return () => collection.clear();
  }, [editor, review, path]);

  const add = async () => {
    if (!writing || !draft.trim()) return;
    try {
      setReview(
        await api.comment(server, name, {
          file: path,
          line: writing.line,
          excerpt: writing.excerpt,
          body: draft,
        }),
      );
      setWriting(null);
      setDraft("");
    } catch (e) {
      setError(messageOf(e));
    }
  };

  if (error && !sides) return <p className="error">{error}</p>;
  if (!sides) return <p className="loading">reading {path}…</p>;
  if (sides.binary) return <p className="loading">{path} is binary</p>;

  const mine = review.filter((c) => c.file === path);

  return (
    <div className="filediff">
      <header>
        <span className="path">{path}</span>
        <span className="sides">
          {sides.original_label} → {sides.modified_label}
        </span>
      </header>
      {error && <p className="error">{error}</p>}
      <div className="monaco" ref={host} />

      {writing && (
        <div className="composer">
          <textarea
            autoFocus
            rows={2}
            value={draft}
            placeholder={`comment on ${path}:${writing.line}`}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                void add();
              }
              if (e.key === "Escape") setWriting(null);
            }}
          />
          <button className="quiet" onClick={() => void add()}>
            add
          </button>
          <button className="quiet" onClick={() => setWriting(null)}>
            cancel
          </button>
        </div>
      )}

      {mine.length > 0 && (
        <ul className="comments">
          {mine.map((c) => (
            <li key={c.id}>
              <span className="at">line {c.line}</span>
              <span className="body">{c.body}</span>
              <button
                className="quiet"
                onClick={() =>
                  api
                    .uncomment(server, name, c.id)
                    .then(setReview)
                    .catch((e) => setError(messageOf(e)))
                }
              >
                remove
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
