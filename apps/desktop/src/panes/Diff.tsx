// What the agent has changed, and what you want to say about it.
//
// Three sections, from `ops::repo_diff`: committed work against the base
// branch, uncommitted work, and untracked files. The body arrives marked up
// rather than structured -- `### ` for a heading, `!!! ` for a notice, and a
// unified diff otherwise -- and `sbx_core::pane` calls those markers a contract
// with whatever draws it. This is the second thing that draws it; the TUI's
// `diff_line` is the first, and they strip the same two prefixes.
//
// The comments are the half with no equivalent in a code host: they are not
// going to a pull request, they are going to the agent that is still running.
// So a review is written a line at a time and sent in one message -- see
// `sbx_core::comments` for why it is one and why the server keeps it.

import { useCallback, useEffect, useState } from "react";

import { api, messageOf } from "../api";
import type { Comment } from "../gen/Comment";

const SECTION = "### ";
const NOTICE = "!!! ";

export function DiffPane({ server, name }: { server: string; name: string }) {
  const [body, setBody] = useState<string | null>(null);
  const [review, setReview] = useState<Comment[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [sent, setSent] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Both on arrival, and again after anything changes the review: the pending
  // marks in the gutter are drawn from the same list the strip below counts.
  const load = useCallback(() => {
    Promise.all([api.diff(server, name), api.comments(server, name)])
      .then(([b, c]) => {
        setBody(b);
        setReview(c);
      })
      .catch((e) => setError(messageOf(e)));
  }, [server, name]);

  useEffect(load, [load]);

  const send = async () => {
    setBusy(true);
    setError(null);
    try {
      setSent(await api.sendComments(server, name));
      setReview([]);
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  };

  if (error && !body) return <p className="error">{error}</p>;
  if (body === null) return <p className="loading">reading the diff…</p>;

  const rows = parse(body);

  return (
    <div className="diff">
      {error && <p className="error">{error}</p>}
      {sent && (
        <details className="notice sent">
          <summary>sent to the agent</summary>
          <pre>{sent}</pre>
        </details>
      )}

      <div className="hunks">
        {rows.map((row, i) => (
          <Row
            key={i}
            row={row}
            comments={review.filter((c) => c.file === row.file && c.line === row.line)}
            onAdd={async (bodyText) => {
              setReview(
                await api.comment(server, name, {
                  file: row.file,
                  line: row.line,
                  excerpt: row.text,
                  body: bodyText,
                }),
              );
            }}
            onRemove={async (id) => setReview(await api.uncomment(server, name, id))}
          />
        ))}
      </div>

      {review.length > 0 && (
        <div className="review">
          <span>
            {review.length} comment{review.length === 1 ? "" : "s"} waiting
          </span>
          <button className="go" disabled={busy} onClick={() => void send()}>
            {busy ? "sending…" : "send to the agent"}
          </button>
        </div>
      )}
    </div>
  );
}

/// One line of the body, with enough about it to hang a comment on.
type Row = {
  kind: "section" | "notice" | "file" | "hunk" | "add" | "del" | "context" | "meta";
  text: string;
  /// The file this line belongs to, without the `a/` or `b/` prefix. Empty
  /// outside any file, which is what makes a line uncommentable.
  file: string;
  /// Line number in the file. Zero where there is not one -- a hunk header, or
  /// an untracked path, which has no lines yet to point at.
  line: number;
};

/// Turn the marked-up body into rows.
///
/// The line numbers come from the hunk headers, counted forward exactly as git
/// wrote them: an added or context line advances the new-file counter, a removed
/// line advances the old one. A comment on a removed line is numbered from the
/// old file, which is the only number that line has.
export function parse(body: string): Row[] {
  const rows: Row[] = [];
  let file = "";
  let oldLine = 0;
  let newLine = 0;
  let untracked = false;

  for (const text of body.split("\n")) {
    if (text.startsWith(SECTION)) {
      const title = text.slice(SECTION.length);
      // The untracked section is a list of paths, not a diff: each line is its
      // own file, and none of them has line numbers.
      untracked = title.startsWith("untracked");
      file = "";
      rows.push({ kind: "section", text: title, file: "", line: 0 });
      continue;
    }
    if (text.startsWith(NOTICE)) {
      rows.push({ kind: "notice", text: text.slice(NOTICE.length), file: "", line: 0 });
      continue;
    }
    if (untracked) {
      // A path on its own. Commentable against the file, with no line.
      const path = text.trim();
      rows.push({ kind: path ? "file" : "meta", text, file: path, line: 0 });
      continue;
    }
    if (text.startsWith("diff --git ")) {
      file = fileOf(text);
      rows.push({ kind: "file", text, file, line: 0 });
      continue;
    }
    const hunk = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(text);
    if (hunk) {
      oldLine = Number(hunk[1]);
      newLine = Number(hunk[2]);
      rows.push({ kind: "hunk", text, file, line: 0 });
      continue;
    }
    if (text.startsWith("+++") || text.startsWith("---") || text.startsWith("index ")) {
      rows.push({ kind: "meta", text, file, line: 0 });
      continue;
    }
    if (text.startsWith("+")) {
      rows.push({ kind: "add", text, file, line: newLine });
      newLine++;
      continue;
    }
    if (text.startsWith("-")) {
      rows.push({ kind: "del", text, file, line: oldLine });
      oldLine++;
      continue;
    }
    rows.push({ kind: "context", text, file, line: newLine });
    if (text.startsWith(" ")) {
      oldLine++;
      newLine++;
    }
  }
  return rows;
}

/// `diff --git a/src/x.rs b/src/x.rs` -> `src/x.rs`.
///
/// Taken from the *second* path, so a rename is commented on under the name the
/// file now has, which is the one the agent will look for.
function fileOf(header: string): string {
  const parts = header.slice("diff --git ".length).split(" ");
  const to = parts[parts.length - 1] ?? "";
  return to.replace(/^b\//, "");
}

function Row({
  row,
  comments,
  onAdd,
  onRemove,
}: {
  row: Row;
  comments: Comment[];
  onAdd: (body: string) => Promise<void>;
  onRemove: (id: number) => Promise<void>;
}) {
  const [writing, setWriting] = useState(false);
  const [draft, setDraft] = useState("");
  const commentable = row.file !== "" && row.kind !== "meta";

  if (row.kind === "section") return <h3 className="section">{row.text}</h3>;
  if (row.kind === "notice") return <p className="notice">{row.text}</p>;

  const submit = async () => {
    if (!draft.trim()) return;
    await onAdd(draft);
    setDraft("");
    setWriting(false);
  };

  return (
    <>
      <div
        className={`line ${row.kind}${commentable ? " open" : ""}`}
        // The whole line is the target rather than a margin affordance: there
        // is no hover on a touch screen and no room for a gutter at this size.
        onClick={commentable ? () => setWriting((w) => !w) : undefined}
        title={commentable ? `comment on ${row.file}${row.line ? `:${row.line}` : ""}` : undefined}
      >
        <span className="num">{row.line || ""}</span>
        <span className="text">{row.text}</span>
      </div>

      {comments.map((c) => (
        <div className="comment" key={c.id}>
          <p>{c.body}</p>
          <button className="quiet" onClick={() => void onRemove(c.id)}>
            remove
          </button>
        </div>
      ))}

      {writing && (
        <div className="composer">
          <textarea
            autoFocus
            rows={2}
            value={draft}
            placeholder={`comment on ${row.file}${row.line ? `:${row.line}` : ""}`}
            onChange={(e) => setDraft(e.target.value)}
            // Enter sends, Shift+Enter breaks the line: a review comment is
            // usually one sentence, and reaching for a button for every one of
            // them is what makes reviewing in a window slower than in a
            // terminal.
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                void submit();
              }
              if (e.key === "Escape") setWriting(false);
            }}
          />
          <button className="quiet" onClick={() => void submit()}>
            add
          </button>
        </div>
      )}
    </>
  );
}
