// Starting a session: pick a repository, then fill in the rest.
//
// Two stages rather than one screen, for the reason `tui/create.rs` gives:
// the picker answers *which repository*, which is a search, and the form
// answers *what kind of session*, which is a handful of fields with defaults
// good enough to submit on sight.
//
// The repository is a way of naming a **remote**. The sandbox clones `origin`
// over the gateway exactly as `sbx new --repo <url>` does, so a checkout with
// no origin cannot start a session, and local edits and unpushed commits stay
// where they are -- which is why the form says how many of each there are.
//
// Nothing here decides anything the other two front ends decide differently.
// The name is derived by the server when this leaves it blank, the policy list
// and the ticked toolchains arrive from the server, and the skills and MCP
// servers are shown rather than offered, because they are one decision made in
// the config file.

import { useEffect, useMemo, useRef, useState } from "react";

import { api, messageOf } from "./api";
import type { Facts } from "./gen/Facts";
import type { Picked } from "./gen/Picked";
import type { Listing } from "./gen/Listing";
import type { LocalRepo } from "./gen/LocalRepo";
import type { NewOptions } from "./gen/NewOptions";

export function NewSessionDialog({
  server,
  onClose,
  onCreated,
}: {
  server: string;
  onClose: () => void;
  onCreated: (name: string) => void;
}) {
  const [listing, setListing] = useState<Listing | null>(null);
  const [options, setOptions] = useState<NewOptions | null>(null);
  const [picked, setPicked] = useState<LocalRepo | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Both at once: the form cannot be drawn without either, and asking in
  // sequence is two chances to show half a dialog.
  useEffect(() => {
    let live = true;
    Promise.all([api.repos(server), api.newOptions(server)])
      .then(([l, o]) => {
        if (!live) return;
        setListing(l);
        setOptions(o);
      })
      .catch((e) => live && setError(messageOf(e)));
    return () => {
      live = false;
    };
  }, [server]);

  // Escape closes from either stage. On the form it steps back to the picker
  // instead, which is the only way back to a different repository.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (picked) setPicked(null);
      else onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [picked, onClose]);

  return (
    <div className="scrim" onMouseDown={onClose}>
      <div className="dialog" onMouseDown={(e) => e.stopPropagation()}>
        {error && <p className="error">{error}</p>}
        {!error && (!listing || !options) && <p className="loading">looking for repositories…</p>}
        {listing && options && !picked && (
          <Picker listing={listing} onPick={setPicked} onClose={onClose} />
        )}
        {listing && options && picked && (
          <Form
            server={server}
            repo={picked}
            options={options}
            onBack={() => setPicked(null)}
            onCreated={onCreated}
          />
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
  onPick,
  onClose,
}: {
  listing: Listing;
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
        <h2>New session</h2>
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
                disabled={!r.origin}
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

/// What kind of session.
function Form({
  server,
  repo,
  options,
  onBack,
  onCreated,
}: {
  server: string;
  repo: LocalRepo;
  options: NewOptions;
  onBack: () => void;
  onCreated: (name: string) => void;
}) {
  const [task, setTask] = useState("");
  const [name, setName] = useState("");
  const [base, setBase] = useState(repo.branch ?? options.default_base ?? "");
  const [policy, setPolicy] = useState(options.default_policy);
  const [toolchains, setToolchains] = useState<string[]>([]);
  const [providers, setProviders] = useState<string[]>(options.default_providers);
  const [facts, setFacts] = useState<Facts | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // The repository has already answered two of these questions, so the form
  // arrives with them answered rather than asking. It costs subprocesses and a
  // gateway call on the server, which is why it happens once, here, and not for
  // every row of the picker.
  useEffect(() => {
    let live = true;
    api
      .inspect(server, repo.path, repo.branch)
      .then((picked: Picked) => {
        if (!live) return;
        setFacts(picked.facts);
        setToolchains(picked.facts.toolchains);
        // Empty when the config file names providers, since an explicit list
        // replaces the rule rather than adding to it -- so the defaults already
        // in state stand.
        if (picked.providers.length > 0) setProviders(picked.providers);
        // A branch that has never been pushed cannot be cloned from, so the
        // remote's default is used instead of handing the gateway a clone that
        // is going to fail.
        if (!picked.facts.base_on_remote) setBase("");
      })
      .catch((e) => live && setError(messageOf(e)));
    return () => {
      live = false;
    };
  }, [server, repo.path, repo.branch]);

  const toggle = (list: string[], set: (v: string[]) => void, value: string) =>
    set(list.includes(value) ? list.filter((v) => v !== value) : [...list, value]);

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      const created = await api.create(server, {
        // Blank means "derive it", which is what `sbx new` without `--name`
        // does. The rule lives on the server so there is one of it.
        name: name.trim() || null,
        repo: repo.origin!,
        task,
        base: base.trim() || null,
        policy,
        providers,
        toolchains,
        start: true,
      });
      onCreated(created);
    } catch (e) {
      setError(messageOf(e));
      setBusy(false);
    }
  };

  return (
    <>
      <header className="dialog-head">
        <h2>{repo.name}</h2>
        <button className="quiet" onClick={onBack}>
          another repository
        </button>
      </header>

      <p className="origin">{repo.origin}</p>
      {facts && <Drift facts={facts} />}

      <label>
        <span>task</span>
        <textarea
          autoFocus
          rows={3}
          value={task}
          placeholder="what the agent should do"
          onChange={(e) => setTask(e.target.value)}
        />
      </label>

      <label>
        <span>name</span>
        <input
          value={name}
          placeholder="derived from the task"
          onChange={(e) => setName(e.target.value)}
        />
      </label>

      <label>
        <span>base</span>
        <input
          value={base}
          placeholder="the remote's default branch"
          onChange={(e) => setBase(e.target.value)}
        />
      </label>

      <label>
        <span>policy</span>
        <select value={policy} onChange={(e) => setPolicy(e.target.value)}>
          {options.policies.map((p) => (
            <option key={p.spec} value={p.spec}>
              {p.spec} — {p.summary}
            </option>
          ))}
        </select>
      </label>

      <fieldset>
        <legend>toolchains</legend>
        {options.toolchains.map((t) => (
          <label key={t.name} className="tick">
            <input
              type="checkbox"
              checked={toolchains.includes(t.name)}
              onChange={() => toggle(toolchains, setToolchains, t.name)}
            />
            <span>{t.name}</span>
            <span className="hint">{t.summary}</span>
          </label>
        ))}
      </fieldset>

      <fieldset>
        <legend>providers</legend>
        {options.providers_error && <p className="error">{options.providers_error}</p>}
        {options.providers.length === 0 && !options.providers_error && (
          <p className="hint">the gateway has no credential providers</p>
        )}
        {options.providers.map((p) => (
          <label key={p.name} className="tick">
            <input
              type="checkbox"
              checked={providers.includes(p.name)}
              onChange={() => toggle(providers, setProviders, p.name)}
            />
            <span>{p.name}</span>
            <span className="hint">{p.kind}</span>
          </label>
        ))}
      </fieldset>

      {/* Named, not offered: skills and MCP servers are one decision about what
          your agents can reach, made in the server's config file. Shown so a
          session's tools are not a surprise. */}
      <dl className="carried">
        <dt>skills</dt>
        <dd>{options.skills.join(", ") || <span className="hint">none</span>}</dd>
        <dt>mcp</dt>
        <dd>{options.mcp.join(", ") || <span className="hint">none</span>}</dd>
      </dl>

      {error && <p className="error">{error}</p>}

      <div className="actions">
        <button className="go" disabled={busy || !repo.origin} onClick={() => void submit()}>
          {busy ? "starting…" : "start session"}
        </button>
      </div>
    </>
  );
}

/// What stays behind on the server's checkout.
///
/// The sandbox clones `origin`, so uncommitted work and unpushed commits are
/// not coming with it. Worth saying before the session starts rather than after
/// the agent has failed to find them.
function Drift({ facts }: { facts: Facts }) {
  const bits: string[] = [];
  if (facts.uncommitted > 0) bits.push(`${facts.uncommitted} uncommitted`);
  if (facts.unpushed) bits.push(`${facts.unpushed} unpushed`);
  if (!facts.base_on_remote) bits.push("this branch is not on the remote");
  if (bits.length === 0) return null;
  return <p className="notice">{bits.join(", ")} — the sandbox clones the remote</p>;
}
