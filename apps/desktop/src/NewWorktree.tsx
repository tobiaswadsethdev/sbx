// Starting a worktree: what this one is for.
//
// The second of the two questions the create flow used to ask on one screen.
// The repository is already answered -- it is the project this is being started
// in -- so what is left is the part that differs between one worktree and the
// next, which is a handful of fields with defaults good enough to submit on
// sight.
//
// Nothing here decides anything `sbx new` decides differently. The name is
// derived by the server when this leaves it blank, the policy list and the
// ticked toolchains and credentials arrive from the server, and the skills and
// MCP servers are shown rather than offered, because they are one decision made
// in the config file.

import { useEffect, useState } from "react";

import { api, messageOf } from "./api";
import type { Facts } from "./gen/Facts";
import type { Kind } from "./gen/Kind";
import type { NewOptions } from "./gen/NewOptions";
import type { Picked } from "./gen/Picked";
import type { Project } from "./gen/Project";

export function NewWorktreeDialog({
  server,
  project,
  onClose,
  onCreated,
}: {
  server: string;
  project: Project;
  onClose: () => void;
  onCreated: (name: string) => void;
}) {
  const [options, setOptions] = useState<NewOptions | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    api
      .newOptions(server)
      .then((o) => live && setOptions(o))
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

  return (
    <div className="scrim" onMouseDown={onClose}>
      <div className="dialog" onMouseDown={(e) => e.stopPropagation()}>
        {error && <p className="error">{error}</p>}
        {!options && !error && <p className="loading">reading the options…</p>}
        {options && (
          <Form
            server={server}
            project={project}
            options={options}
            onClose={onClose}
            onCreated={onCreated}
          />
        )}
      </div>
    </div>
  );
}

/// What kind of worktree.
function Form({
  server,
  project,
  options,
  onClose,
  onCreated,
}: {
  server: string;
  project: Project;
  options: NewOptions;
  onClose: () => void;
  onCreated: (name: string) => void;
}) {
  const [task, setTask] = useState("");
  const [name, setName] = useState("");
  // The sandbox is the default here for the same reason it is the default
  // everywhere: it is the point of the tool, and the other one gives up the
  // guarantee it exists to make.
  const [kind, setKind] = useState<Kind>("sandbox");
  const [base, setBase] = useState(options.default_base ?? "");
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
      // The project stores a path; which branch that checkout is on is a fact
      // only the server can read, so it comes back with the rest.
      .inspect(server, project.path, null)
      .then((picked: Picked) => {
        if (!live) return;
        setFacts(picked.facts);
        setToolchains(picked.facts.toolchains);
        if (picked.branch) setBase(picked.branch);
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
  }, [server, project.path]);

  const toggle = (list: string[], set: (v: string[]) => void, value: string) =>
    set(list.includes(value) ? list.filter((v) => v !== value) : [...list, value]);

  const sandboxed = kind === "sandbox";

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      const created = await api.create(server, {
        backend: kind,
        project: project.name,
        // Blank means "derive it", which is what `sbx new` without `--name`
        // does. The rule lives on the server so there is one of it.
        name: name.trim() || null,
        repo: project.repo,
        task,
        base: base.trim() || null,
        // Sent as they stand for a sandbox, and empty for a worktree: there is
        // no gateway to give a policy to, no credential for it to swap in and
        // no image to build a toolchain into. The server clears them on the
        // record too, so a worktree session never claims to have had any.
        policy,
        providers: sandboxed ? providers : [],
        toolchains: sandboxed ? toolchains : [],
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
        <h2>{project.name}</h2>
        <button className="quiet" onClick={onClose}>
          close
        </button>
      </header>

      <p className="origin">{project.repo}</p>
      {facts && <Drift facts={facts} sandboxed={sandboxed} />}

      {/* The first question, because it decides which of the others mean
          anything. Two radios rather than a select: there are two, and what
          matters about the second one is the sentence beside it. */}
      <fieldset className="kinds">
        <legend>where it runs</legend>
        <label className="tick">
          <input
            type="radio"
            checked={sandboxed}
            onChange={() => setKind("sandbox")}
          />
          <span>sandbox</span>
          <span className="hint">
            a kernel-enforced sandbox, cloned from the remote, with a policy on
            everything it reaches
          </span>
        </label>
        <label className="tick">
          <input
            type="radio"
            checked={!sandboxed}
            onChange={() => setKind("worktree")}
          />
          <span>worktree</span>
          <span className="hint">
            a `git worktree` on the server, in seconds, sharing this checkout's
            history
          </span>
        </label>
      </fieldset>

      {!sandboxed && (
        <p className="warn">
          No isolation: the agent runs on the server as the server's user, with
          its files, its git credentials and whatever the network allows. There
          is no policy and no allow/deny feed for this session.
        </p>
      )}

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
          placeholder={sandboxed ? "the remote's default branch" : "the checkout's branch"}
          onChange={(e) => setBase(e.target.value)}
        />
      </label>

      {/* Everything below is an instruction to a gateway, so a worktree
          session is not asked any of it. Hidden rather than disabled: a
          greyed-out policy chooser suggests the choice exists and has been
          taken away, when the truth is that there is nothing to apply it to. */}
      {sandboxed ? (
        <>
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

          {/* Named, not offered: skills and MCP servers are one decision about
              what your agents can reach, made in the server's config file.
              Shown so a session's tools are not a surprise. */}
          <dl className="carried">
            <dt>skills</dt>
            <dd>{options.skills.join(", ") || <span className="hint">none</span>}</dd>
            <dt>mcp</dt>
            <dd>{options.mcp.join(", ") || <span className="hint">none</span>}</dd>
          </dl>
        </>
      ) : (
        <p className="hint">
          The agent is the server's own, so it reads the server user's own
          skills and MCP servers rather than being given a copy of them.
        </p>
      )}

      {error && <p className="error">{error}</p>}

      <div className="actions">
        <button className="go" disabled={busy} onClick={() => void submit()}>
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
///
/// A worktree is a different answer to the same question and gets a different
/// sentence: it shares the object store, so unpushed commits *are* there and a
/// base branch that has never been pushed is a perfectly good base -- but the
/// working copy is its own, so uncommitted changes stay where they are.
function Drift({ facts, sandboxed }: { facts: Facts; sandboxed: boolean }) {
  const bits: string[] = [];
  if (facts.uncommitted > 0) bits.push(`${facts.uncommitted} uncommitted`);
  if (sandboxed) {
    if (facts.unpushed) bits.push(`${facts.unpushed} unpushed`);
    if (!facts.base_on_remote) bits.push("this branch is not on the remote");
  }
  if (bits.length === 0) return null;
  return (
    <p className="notice">
      {bits.join(", ")} —{" "}
      {sandboxed
        ? "the sandbox clones the remote"
        : "a worktree shares this checkout's history, not its working copy"}
    </p>
  );
}
