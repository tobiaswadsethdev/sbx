// What the server holds on your sessions' behalf: MCP servers, the secrets they
// need, and the skills this machine has pushed to it.
//
// Three things that used to be three procedures in a document. An MCP server was
// a `docker run` line to copy, with the credential on it, re-typed after every
// reboot; its secret was a `-e` argument in somebody's shell history; a skill
// was a path in the server's config file, which cannot reach a laptop's
// `~/.claude/skills` at all. Each is now a row with a button.
//
// **Every action re-reads the whole view rather than adjusting this list.** The
// same decision the git view made, for the same reason: these explain each
// other. Storing a secret is usually what a container was waiting for, and a
// client that patched its own copy would be inventing the answer.

import { useEffect, useState } from "react";

import { api, messageOf } from "./api";
import type { Integrations as View } from "./gen/Integrations";
import type { McpStatus } from "./gen/McpStatus";
import type { NamedSecret } from "./gen/NamedSecret";
import { Close } from "./icons";

export function IntegrationsDialog({
  server,
  onClose,
}: {
  server: string;
  onClose: () => void;
}) {
  const [view, setView] = useState<View | null>(null);
  const [mine, setMine] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    api
      .integrations(server)
      .then((v) => live && setView(v))
      .catch((e) => live && setError(messageOf(e)));
    // What *this* machine has to upload, read on the Rust side of the bridge:
    // `~/.claude/skills` is here, and a webview cannot see it.
    api.mySkills().then((names) => live && setMine(names));
    return () => {
      live = false;
    };
  }, [server]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  /// One action, its answer, and whatever went wrong with it.
  ///
  /// Every one of these returns the view, so this is also what keeps the screen
  /// current -- there is no refresh button and nothing to poll.
  const act = async (what: string, run: () => Promise<View>) => {
    setBusy(what);
    setError(null);
    try {
      setView(await run());
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="scrim" onMouseDown={onClose}>
      <div className="dialog wide" onMouseDown={(e) => e.stopPropagation()}>
        <header className="dialog-head">
          <h2>Integrations</h2>
          <button className="quiet" onClick={onClose}>
            close
          </button>
        </header>

        {error && <p className="error">{error}</p>}
        {!view && !error && <p className="loading">asking the server…</p>}

        {view && (
          <>
            <Section
              title="mcp servers"
              hint="Tools the agent can call. They run on the server, in their own containers, holding their own credentials — the sandbox is granted one endpoint each."
            >
              {view.mcp.length === 0 ? (
                <p className="hint">
                  None. Add an <code>[[mcp]]</code> table to the server's config file; see
                  docs/mcp.md.
                </p>
              ) : (
                view.mcp.map((s) => (
                  <McpRow
                    key={s.name}
                    status={s}
                    busy={busy}
                    onAction={(action) =>
                      void act(`mcp:${s.name}`, () => api.mcp(server, s.name, action))
                    }
                  />
                ))
              )}
              {/* The warning that used to live in a document nobody re-reads,
                  at the moment somebody is looking at the thing it is about. */}
              <p className="warn">
                An MCP server is something the agent can do with your credentials. The gateway sees
                every call as <code>POST /mcp</code>, so there is no finer rule than granting the
                endpoint: a server that can transition Jira issues means a sandboxed agent can
                transition Jira issues. Fine for Jira; a filesystem or Docker server would be a
                straight way out of the sandbox.
              </p>
            </Section>

            <Section
              title="secrets"
              hint="Held by the server and given to the containers above as environment. A value goes in and never comes back out — nothing here can show you one."
            >
              {view.secrets.length === 0 && <p className="hint">None stored, and none asked for.</p>}
              {view.secrets.map((s) => (
                <SecretRow
                  key={s.name}
                  secret={s}
                  busy={busy}
                  onSet={(value) =>
                    void act(`secret:${s.name}`, () => api.secret(server, s.name, value))
                  }
                  onForget={() =>
                    void act(`secret:${s.name}`, () => api.secret(server, s.name, null))
                  }
                />
              ))}
              <NewSecret
                busy={busy !== null}
                onSet={(name, value) =>
                  void act(`secret:${name}`, () => api.secret(server, name, value))
                }
              />
            </Section>

            <Section
              title="skills"
              hint="Copied into every new session. The server keeps a library of what this machine has pushed to it; the originals stay here, and pushing again is how an edit reaches the next session."
            >
              {view.configured_skills.length > 0 && (
                <p className="hint">
                  From the server's own config file: {view.configured_skills.join(", ")}
                </p>
              )}
              {view.skills.length === 0 ? (
                <p className="hint">Nothing uploaded yet.</p>
              ) : (
                view.skills.map((s) => (
                  <div key={s.name} className="row">
                    <span className="row-name">{s.name}</span>
                    <span className="hint" title={s.origin}>
                      {s.origin}
                    </span>
                    <button
                      className="quiet"
                      disabled={busy !== null}
                      title="remove it from the server (your own copy stays)"
                      onClick={() =>
                        void act(`skill:${s.name}`, () => api.forgetSkill(server, s.name))
                      }
                    >
                      <Close aria-label="forget" />
                    </button>
                  </div>
                ))
              )}
              <div className="actions">
                <span className="hint">
                  {mine.length > 0
                    ? `${mine.length} here: ${mine.join(", ")}`
                    : "no skills in ~/.claude/skills on this machine"}
                </span>
                <button
                  className="go"
                  disabled={busy !== null || mine.length === 0}
                  onClick={() => void act("upload", () => api.uploadSkills(server))}
                >
                  {busy === "upload" ? "uploading…" : "push mine to the server"}
                </button>
              </div>
            </Section>
          </>
        )}
      </div>
    </div>
  );
}

function Section({
  title,
  hint,
  children,
}: {
  title: string;
  hint: string;
  children: React.ReactNode;
}) {
  return (
    <section className="integration">
      <h3>{title}</h3>
      <p className="hint">{hint}</p>
      {children}
    </section>
  );
}

/// One MCP server: what it is, what it is doing, and what to press.
///
/// The state and the words come from the server -- `mcp::Status` -- so this and
/// `sbxd mcp` and `sbx doctor` cannot disagree about whether something is
/// running.
function McpRow({
  status,
  busy,
  onAction,
}: {
  status: McpStatus;
  busy: string | null;
  onAction: (action: "start" | "restart" | "stop") => void;
}) {
  const working = busy === `mcp:${status.name}`;
  const state = status.managed ? status.state : "external";
  return (
    <div className="row mcp">
      <span className="row-name">{status.name}</span>
      <span className={`mcp-state ${state}`}>{state}</span>
      <span className="hint" title={status.url}>
        {status.image ?? status.url}
      </span>
      {status.managed ? (
        <span className="row-actions">
          {/* Start when there is nothing running, restart when there is: the
              two are different actions on the server -- start leaves a healthy
              container alone, restart recreates it from the catalog, which is
              what to press after changing a secret. */}
          {status.state === "running" ? (
            <>
              <button className="quiet" disabled={working} onClick={() => onAction("restart")}>
                restart
              </button>
              <button className="quiet" disabled={working} onClick={() => onAction("stop")}>
                stop
              </button>
            </>
          ) : (
            <button className="quiet" disabled={working} onClick={() => onAction("start")}>
              {working ? "starting…" : "start"}
            </button>
          )}
        </span>
      ) : (
        // Nothing to press: whoever runs it started it, and this server has no
        // say in whether it is up.
        <span className="row-actions hint">not ours to start</span>
      )}
      {status.problem && <p className="problem">{status.problem}</p>}
      {/* The container's own last words, which are the only thing that ever
          says why an image will not stay up -- and otherwise a `docker logs` on
          a machine you may not be sitting at. */}
      {status.log && <pre className="log">{status.log}</pre>}
    </div>
  );
}

function SecretRow({
  secret,
  busy,
  onSet,
  onForget,
}: {
  secret: NamedSecret;
  busy: string | null;
  onSet: (value: string) => void;
  onForget: () => void;
}) {
  const [value, setValue] = useState("");
  const working = busy === `secret:${secret.name}`;
  return (
    <div className="row secret">
      <span className="row-name">{secret.name}</span>
      <span className={secret.set ? "yes" : "no"}>{secret.set ? "stored" : "NOT set"}</span>
      <span className="hint">
        {secret.used_by.length > 0 ? `used by ${secret.used_by.join(", ")}` : "nothing uses it"}
      </span>
      <span className="row-actions">
        <input
          type="password"
          value={value}
          placeholder={secret.set ? "replace it" : "paste the value"}
          onChange={(e) => setValue(e.target.value)}
        />
        <button
          className="quiet"
          disabled={working || value.trim().length === 0}
          onClick={() => {
            onSet(value);
            setValue("");
          }}
        >
          store
        </button>
        {secret.set && (
          <button className="quiet" disabled={working} onClick={onForget} title="forget it">
            <Close aria-label="forget" />
          </button>
        )}
      </span>
    </div>
  );
}

/// A name the catalog does not mention yet.
///
/// Worth having even though a catalog entry is what creates a row above: the
/// ordinary order is to add the `[[mcp]]` table and restart the server, and
/// storing the secret first means the container comes up working the first time.
function NewSecret({
  busy,
  onSet,
}: {
  busy: boolean;
  onSet: (name: string, value: string) => void;
}) {
  const [name, setName] = useState("");
  const [value, setValue] = useState("");
  const ready = /^[A-Za-z_][A-Za-z0-9_]*$/.test(name.trim()) && value.length > 0;
  return (
    <div className="row secret new">
      <input
        className="secret-name"
        value={name}
        placeholder="ANOTHER_TOKEN"
        onChange={(e) => setName(e.target.value)}
      />
      <input
        type="password"
        value={value}
        placeholder="the value"
        onChange={(e) => setValue(e.target.value)}
      />
      <button
        className="quiet"
        disabled={busy || !ready}
        onClick={() => {
          onSet(name.trim(), value);
          setName("");
          setValue("");
        }}
      >
        store
      </button>
    </div>
  );
}
