// Making a project: choosing the repository you are going to work on.
//
// The picker that used to open every time a session was created. It opens once
// per repository now, because a project is a standing answer to "which
// repository" -- and the question worth asking when you start work is the one
// that varies, which is what the worktree is for.
//
// The repositories are the **server's**. A checkout only ever names a remote --
// the sandbox clones `origin` over the gateway either way -- but which
// checkouts exist is a fact about the machine that will do the cloning, and
// `repo_roots` is configured there.

import { useEffect, useMemo, useRef, useState } from "react";

import { api, messageOf } from "./api";
import type { Listing } from "./gen/Listing";
import type { LocalRepo } from "./gen/LocalRepo";
import type { Project } from "./gen/Project";

export function NewProjectDialog({
  server,
  onClose,
  onCreated,
}: {
  server: string;
  onClose: () => void;
  onCreated: (projects: Project[]) => void;
}) {
  const [listing, setListing] = useState<Listing | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let live = true;
    api
      .repos(server)
      .then((l) => live && setListing(l))
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

  const pick = async (repo: LocalRepo) => {
    setBusy(true);
    setError(null);
    try {
      onCreated(await api.newProject(server, { path: repo.path, repo: repo.origin!, name: null }));
    } catch (e) {
      setError(messageOf(e));
      setBusy(false);
    }
  };

  return (
    <div className="scrim" onMouseDown={onClose}>
      <div className="dialog" onMouseDown={(e) => e.stopPropagation()}>
        {error && <p className="error">{error}</p>}
        {!listing && !error && <p className="loading">looking for repositories…</p>}
        {listing && (
          <Picker listing={listing} busy={busy} onPick={(r) => void pick(r)} onClose={onClose} />
        )}
      </div>
    </div>
  );
}

/// Which repository.
///
/// The filter is a substring match, where the TUI ranks with the fuzzy score in
/// `repos::score`. Not an oversight: the alternative to a second copy of that
/// scorer in TypeScript is a request per keystroke, and of the three options a
/// plainer match on the same list is the one that cannot go quietly wrong. If
/// the two ever need to agree exactly, the scorer moves to the server.
function Picker({
  listing,
  busy,
  onPick,
  onClose,
}: {
  listing: Listing;
  busy: boolean;
  onPick: (repo: LocalRepo) => void;
  onClose: () => void;
}) {
  const [query, setQuery] = useState("");
  const field = useRef<HTMLInputElement>(null);

  useEffect(() => field.current?.focus(), []);

  const rows = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return listing.repos;
    return listing.repos.filter((r) => r.display.toLowerCase().includes(needle));
  }, [listing.repos, query]);

  return (
    <>
      <header className="dialog-head">
        <h2>New project</h2>
        <button className="quiet" onClick={onClose}>
          close
        </button>
      </header>
      <input
        ref={field}
        className="search"
        placeholder="filter repositories"
        value={query}
        onChange={(e) => setQuery(e.target.value)}
      />
      {listing.repos.length === 0 ? (
        <div className="none">
          <p>No git repositories on the server.</p>
          <p>
            It looked in {listing.roots.join(", ") || "nowhere"}. Set{" "}
            <code>repo_roots</code> in its config file to look elsewhere.
          </p>
        </div>
      ) : (
        <ul className="repos">
          {rows.map((r) => (
            <li key={r.path}>
              <button
                // A checkout with no origin has nothing for the sandbox to
                // clone. Shown and refused rather than hidden, which is clearer
                // than a repository that is simply missing from the list.
                disabled={!r.origin || busy}
                onClick={() => onPick(r)}
              >
                <span className="name">{r.name}</span>
                <span className="branch">{r.branch ?? "detached"}</span>
                <span className="path">{r.display}</span>
                <span className="origin">{r.origin ?? "no origin — cannot be cloned"}</span>
              </button>
            </li>
          ))}
          {rows.length === 0 && <li className="none">nothing matches “{query}”</li>}
        </ul>
      )}
    </>
  );
}
