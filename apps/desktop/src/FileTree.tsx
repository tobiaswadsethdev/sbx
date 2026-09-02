// The working copy, as a tree.
//
// One directory per request, expanded as it is opened: a repository is tens of
// thousands of files, every listing is an exec into the sandbox, and a tree
// only ever shows the few branches someone has opened. Loading it all up front
// would cost a second of execs to draw something nobody has looked at.
//
// Read-only, because the agent owns the working copy. Two writers with no
// shared lock is how a file ends up with half of each; what you want here is to
// see what it did, and the review is how you say something about it.

import { useEffect, useState } from "react";

import { api, messageOf } from "./api";
import type { Entry } from "./gen/Entry";
import { Chevron, FileIcon, Folder } from "./icons";

export function FileTree({
  server,
  name,
  onOpen,
}: {
  server: string;
  name: string;
  onOpen: (path: string) => void;
}) {
  return (
    <div className="files">
      <header>files</header>
      <Level server={server} name={name} path="" depth={0} onOpen={onOpen} />
    </div>
  );
}

/// One directory's worth, and the directories opened under it.
function Level({
  server,
  name,
  path,
  depth,
  onOpen,
}: {
  server: string;
  name: string;
  path: string;
  depth: number;
  onOpen: (path: string) => void;
}) {
  const [entries, setEntries] = useState<Entry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState<Set<string>>(new Set());

  useEffect(() => {
    let live = true;
    setEntries(null);
    api
      .files(server, name, path)
      .then((dir) => live && setEntries(dir.entries))
      .catch((e) => live && setError(messageOf(e)));
    return () => {
      live = false;
    };
  }, [server, name, path]);

  if (error) return <p className="error">{error}</p>;
  if (!entries) return <p className="loading" style={{ paddingLeft: depth * 12 + 8 }}>…</p>;

  return (
    <>
      {entries.map((e) => {
        const full = path ? `${path}/${e.name}` : e.name;
        const isOpen = open.has(full);
        return (
          <div key={full}>
            <button
              className={`entry${e.dir ? " dir" : ""}`}
              style={{ paddingLeft: depth * 12 + 8 }}
              onClick={() => {
                if (!e.dir) return onOpen(full);
                setOpen((s) => {
                  const next = new Set(s);
                  // Collapsing forgets the level, so reopening re-reads it --
                  // the agent is still editing, and a tree that cached what was
                  // there an hour ago would be a tree of what used to be.
                  next.has(full) ? next.delete(full) : next.add(full);
                  return next;
                });
              }}
            >
              <span className="twist">{e.dir && <Chevron open={isOpen} />}</span>
              {e.dir ? <Folder open={isOpen} /> : <FileIcon name={e.name} />}
              <span className="label">{e.name}</span>
            </button>
            {e.dir && isOpen && (
              <Level
                server={server}
                name={name}
                path={full}
                depth={depth + 1}
                onOpen={onOpen}
              />
            )}
          </div>
        );
      })}
      {entries.length === 0 && (
        <p className="loading" style={{ paddingLeft: depth * 12 + 8 }}>
          empty
        </p>
      )}
    </>
  );
}
