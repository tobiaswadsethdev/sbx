// A file, in Monaco.
//
// Read-only: the agent owns the working copy. The editor is here for what it is
// good at -- syntax, folding, find, and a diff view that reads like the one in
// the editor you already use -- not so this becomes a second editor of the same
// files.
//
// Monaco needs a web worker, and this is the one thing about it that fails
// quietly: without one it still renders, and the diff editor shows two panes
// with no red or green in them, which looks like an empty diff rather than a
// missing worker. Measured in WebKitGTK before any of this was built on it --
// see docs/desktop.md, and the terminal above for why that was worth doing.

import { useEffect, useRef, useState } from "react";
// The editor API and the *tokenisers*, not the language services. Importing
// `monaco-editor` whole pulls in IntelliSense for TypeScript, CSS, HTML and
// JSON -- four web workers and about nine megabytes -- to power completions in
// a viewer that cannot be typed into. `basic-languages` is the highlighting on
// its own, which is the part a reader wants.
import * as monaco from "monaco-editor/editor/editor.api";
import "monaco-editor/basic-languages/monaco.contribution";
// Still one worker: it is what computes a diff, and the diff view is half the
// reason for an editor here at all.
import EditorWorker from "monaco-editor/editor/editor.worker?worker";

import { api, messageOf } from "../api";
import type { FileText } from "../gen/FileText";

(self as unknown as { MonacoEnvironment: unknown }).MonacoEnvironment = {
  getWorker: () => new EditorWorker(),
};

/// Matching style.css, so the editor is not a white rectangle in a dark pane.
export const THEME = "sbx-dark";
monaco.editor.defineTheme(THEME, {
  base: "vs-dark",
  inherit: true,
  rules: [],
  colors: { "editor.background": "#0e0e12", "editorGutter.background": "#0e0e12" },
});

export function FilePane({
  server,
  name,
  path,
}: {
  server: string;
  name: string;
  path: string;
}) {
  const host = useRef<HTMLDivElement>(null);
  const [file, setFile] = useState<FileText | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    setFile(null);
    setError(null);
    api
      .file(server, name, path)
      .then((f) => live && setFile(f))
      .catch((e) => live && setError(messageOf(e)));
    return () => {
      live = false;
    };
  }, [server, name, path]);

  useEffect(() => {
    const element = host.current;
    if (!element || !file || file.binary) return;

    const editor = monaco.editor.create(element, {
      value: file.text,
      language: languageOf(path),
      theme: THEME,
      readOnly: true,
      // The working copy is the agent's, and an editor that looks editable is
      // an invitation to type into it and lose the keystrokes.
      domReadOnly: true,
      automaticLayout: true,
      minimap: { enabled: false },
      scrollBeyondLastLine: false,
      fontSize: 13,
    });
    return () => {
      editor.getModel()?.dispose();
      editor.dispose();
    };
  }, [file, path]);

  if (error) return <p className="error">{error}</p>;
  if (!file) return <p className="loading">reading {path}…</p>;
  if (file.binary) {
    return <p className="loading">{path} is binary ({file.bytes} bytes)</p>;
  }

  return (
    <div className="file">
      {file.truncated && (
        <p className="notice">
          showing the first part of {path}; it is {file.bytes} bytes
        </p>
      )}
      <div className="monaco" ref={host} />
    </div>
  );
}

/// Monaco's own id for the language, from the extension.
///
/// A short list rather than a complete one: an unknown extension gets no
/// highlighting, which is exactly what it should get, and a table of two
/// hundred entries would be two hundred chances to be subtly wrong.
export function languageOf(path: string): string {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  const known: Record<string, string> = {
    rs: "rust",
    ts: "typescript",
    tsx: "typescript",
    js: "javascript",
    jsx: "javascript",
    json: "json",
    css: "css",
    html: "html",
    md: "markdown",
    py: "python",
    sh: "shell",
    bash: "shell",
    yaml: "yaml",
    yml: "yaml",
    toml: "ini",
    sql: "sql",
    cs: "csharp",
    go: "go",
    java: "java",
    c: "c",
    h: "c",
    cpp: "cpp",
    hpp: "cpp",
  };
  return known[ext] ?? "plaintext";
}
