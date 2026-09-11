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
import type { ConfiguredTracker } from "./gen/ConfiguredTracker";
import type { Tracker } from "./gen/Tracker";
import type { TrackerKind } from "./gen/TrackerKind";
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
  ///
  /// Answers whether it worked, for the one caller that has to know: a form
  /// clears itself when what it sent was accepted, and keeps what was typed
  /// when it was not.
  const act = async (what: string, run: () => Promise<View>): Promise<boolean> => {
    setBusy(what);
    setError(null);
    try {
      setView(await run());
      return true;
    } catch (e) {
      setError(messageOf(e));
      return false;
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
                  None. An MCP server is an <code>[[mcp]]</code> table in the server's config file;
                  Settings names the file it is reading.
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
              title="trackers"
              hint="Where the inbox gets its tickets. Read on the server with the credential it holds, so this window shows rows and never a token — and a session started from a ticket comments its pull request back onto it."
            >
              {view.trackers.length === 0 && (
                <p className="hint">
                  None, which is the only reason an inbox is ever empty for good. Add one and the
                  inbox has something to read.
                </p>
              )}
              {view.trackers.map((t) => (
                <TrackerRow
                  key={t.source.name}
                  tracker={t}
                  busy={busy}
                  onForget={() =>
                    void act(`tracker:${t.source.name}`, () =>
                      api.forgetTracker(server, t.source.name),
                    )
                  }
                />
              ))}
              <NewTracker
                busy={busy !== null}
                onAdd={(tracker, credential) =>
                  act(`tracker:${tracker.name}`, async () => {
                    // The credential first, so the tracker is never in the file
                    // for a moment with nothing behind the name it gives. Both
                    // answer with the view; the second one is the one kept.
                    if (credential.length > 0) {
                      await api.secret(server, tracker.secret, credential);
                    }
                    return api.addTracker(server, tracker);
                  })
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

/// What a tracker is pointed at, in one line.
///
/// Each kind is addressed differently -- a GitHub repository, an Azure DevOps
/// organisation and project, a Jira site -- and the row says which rather than
/// making three columns that are empty two times out of three.
function target(t: Tracker): string {
  switch (t.kind) {
    case "git-hub":
      return t.repo ?? "everything assigned to you";
    case "azure-dev-ops":
      return [t.org, t.project].filter(Boolean).join("/");
    case "jira":
      return [t.site, t.email].filter(Boolean).join(" as ");
  }
}

/// What a kind is called out here.
///
/// The wire value is serde's kebab-case of the Rust variant -- `git-hub`,
/// `azure-dev-ops` -- and those are not names anybody uses. The config file
/// spells them the way this does, so a row and the file agree.
const KINDS: { value: TrackerKind; label: string }[] = [
  { value: "jira", label: "jira" },
  { value: "azure-dev-ops", label: "azure-devops" },
  { value: "git-hub", label: "github" },
];

function kindLabel(kind: TrackerKind): string {
  return KINDS.find((k) => k.value === kind)?.label ?? kind;
}

/// One configured tracker, and whether the credential it names is actually
/// there.
///
/// The secret is the half that is not in the config file, and a name with
/// nothing behind it is the whole of why an inbox comes back empty with a
/// warning on it -- so it is said here, beside the entry, rather than only in
/// the secrets list above.
function TrackerRow({
  tracker,
  busy,
  onForget,
}: {
  tracker: ConfiguredTracker;
  busy: string | null;
  onForget: () => void;
}) {
  const t = tracker.source;
  const working = busy === `tracker:${t.name}`;
  return (
    <div className="row tracker">
      <span className="row-name">{t.name}</span>
      <span className="tracker-kind">{kindLabel(t.kind)}</span>
      <span className="hint" title={target(t)}>
        {target(t)}
      </span>
      <span className={tracker.secret_set ? "yes" : "no"}>
        {tracker.secret_set ? t.secret : `${t.secret} NOT set`}
      </span>
      <span className="row-actions">
        <button
          className="quiet"
          disabled={working}
          title="remove it from the server's config file (the secret stays)"
          onClick={onForget}
        >
          <Close aria-label="forget" />
        </button>
      </span>
      {/* The entry is in the file and the credential is not, which is a
          working configuration that cannot fetch anything. Said here because
          the inbox's own version of it is a warning on an empty list. */}
      {!tracker.secret_set && (
        <p className="problem">
          Nothing is stored under <code>{t.secret}</code>; store it in the secrets above and this
          tracker starts answering.
        </p>
      )}
    </div>
  );
}

/// Add a tracker: the entry and its credential in one form.
///
/// Two requests underneath -- the secret, then the table -- because they are
/// two different things on the server: the value goes into a store this window
/// cannot read back, and the name of it goes into the config file. One form,
/// because "add a tracker" is one intention and a screen that made you do it in
/// two halves would be the documentation this replaced.
function NewTracker({
  busy,
  onAdd,
}: {
  busy: boolean;
  /// Answers whether the server took it. A rejected entry keeps what was typed
  /// -- the reason it was rejected is one field, and re-typing the other six
  /// would be the punishment for a typo.
  onAdd: (tracker: Tracker, credential: string) => Promise<boolean>;
}) {
  const [kind, setKind] = useState<TrackerKind>("jira");
  const [name, setName] = useState("");
  const [secret, setSecret] = useState("");
  const [credential, setCredential] = useState("");
  const [repo, setRepo] = useState("");
  const [org, setOrg] = useState("");
  const [project, setProject] = useState("");
  const [site, setSite] = useState("");
  const [email, setEmail] = useState("");
  const [query, setQuery] = useState("");
  const [onPublish, setOnPublish] = useState("");

  const blank = (v: string) => (v.trim().length > 0 ? v.trim() : null);
  // What each kind cannot work without. The same rule the server enforces when
  // it parses the file -- checked here as well so the answer is immediate, and
  // there rather than here because the file can also be written by hand.
  const ready =
    secret.trim().length > 0 &&
    (kind !== "jira" || (site.trim().length > 0 && email.trim().length > 0)) &&
    (kind !== "azure-dev-ops" || (org.trim().length > 0 && project.trim().length > 0));

  const tracker = (): Tracker => ({
    kind,
    // Blank means "call it after its kind", which is what the server does with
    // a table that has no `name`.
    name: name.trim(),
    secret: secret.trim(),
    repo: kind === "git-hub" ? blank(repo) : null,
    org: kind === "azure-dev-ops" ? blank(org) : null,
    project: kind === "azure-dev-ops" ? blank(project) : null,
    site: kind === "jira" ? blank(site) : null,
    email: kind === "jira" ? blank(email) : null,
    query: blank(query),
    on_publish: blank(onPublish),
  });

  return (
    <div className="row tracker new">
      <select value={kind} onChange={(e) => setKind(e.target.value as TrackerKind)}>
        {KINDS.map((k) => (
          <option key={k.value} value={k.value}>
            {k.label}
          </option>
        ))}
      </select>
      <input
        value={name}
        placeholder={kindLabel(kind)}
        onChange={(e) => setName(e.target.value)}
      />

      {kind === "git-hub" && (
        <input value={repo} placeholder="owner/name" onChange={(e) => setRepo(e.target.value)} />
      )}
      {kind === "azure-dev-ops" && (
        <>
          <input value={org} placeholder="organisation" onChange={(e) => setOrg(e.target.value)} />
          <input
            value={project}
            placeholder="project"
            onChange={(e) => setProject(e.target.value)}
          />
        </>
      )}
      {kind === "jira" && (
        <>
          <input
            value={site}
            placeholder="https://you.atlassian.net"
            onChange={(e) => setSite(e.target.value)}
          />
          {/* Jira Cloud is Basic auth with the email as the username, so a
              token on its own authenticates as nobody. */}
          <input
            value={email}
            placeholder="you@example.com"
            onChange={(e) => setEmail(e.target.value)}
          />
        </>
      )}

      <input
        className="secret-name"
        value={secret}
        placeholder="SECRET_NAME"
        onChange={(e) => setSecret(e.target.value)}
      />
      <input
        type="password"
        value={credential}
        placeholder="the token (optional here)"
        onChange={(e) => setCredential(e.target.value)}
      />
      <input
        value={query}
        placeholder={
          kind === "jira"
            ? "JQL (optional)"
            : kind === "git-hub"
              ? "search (optional)"
              : "WIQL (optional)"
        }
        onChange={(e) => setQuery(e.target.value)}
      />
      <input
        value={onPublish}
        placeholder="move to, on publish (optional)"
        onChange={(e) => setOnPublish(e.target.value)}
      />
      <span className="row-actions">
        <button
          className="go"
          disabled={busy || !ready}
          onClick={() => {
            void onAdd(tracker(), credential).then((added) => {
              // The credential goes either way: it is in the server's store now
              // if this worked, and a password field holding a token while the
              // screen is open is worth nothing to anybody.
              setCredential("");
              if (!added) return;
              setName("");
              setSecret("");
              setRepo("");
              setOrg("");
              setProject("");
              setSite("");
              setEmail("");
              setQuery("");
              setOnPublish("");
            });
          }}
        >
          add
        </button>
      </span>
      <p className="hint">
        The token is stored under the name beside it and stays on the server. Leave it out and the
        entry is added anyway — the secrets above are where it can be stored later.
      </p>
    </div>
  );
}
