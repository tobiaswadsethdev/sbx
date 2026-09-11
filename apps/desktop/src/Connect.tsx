// The servers this machine is paired with, and pairing with another.
//
// A pairing string is an address, a token and the fingerprint of the
// certificate the server will present, and pasting one here does exactly what
// `sbx connect` does with it -- the checks and the saving are
// `sbx_client::pair` on the Rust side, called by both. What this adds is that
// the machine running the window does not need a CLI: on Windows there is no
// `sbx` to install, because the half of it that drives sandboxes needs a
// gateway and a Docker daemon that only exist on the Linux side.
//
// The string carries a credential, so it is never echoed back into an error
// message and never logged. What comes back on success is the server's own
// version, which is the one thing a paste cannot fake: it arrived over the
// pinned connection the fingerprint in that string describes.
//
// **It is the one screen that works with no server selected**, which is why it
// is a screen and not a dialog over the workspace: on a first run there is no
// workspace to be over. `App` renders it on its own in that case.

import { useEffect, useRef, useState } from "react";

import { api, messageOf, type Paired, type ServerSummary } from "./api";
import { Forget, Servers } from "./icons";
import { Screen } from "./Screen";

export function ServersScreen({
  servers,
  onClose,
  onPaired,
  onForgot,
}: {
  servers: ServerSummary[];
  onClose: () => void;
  onPaired: (paired: Paired) => void;
  onForgot: (servers: ServerSummary[]) => void;
}) {
  const [pairing, setPairing] = useState("");
  const [name, setName] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const field = useRef<HTMLTextAreaElement>(null);

  useEffect(() => field.current?.focus(), []);

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      onPaired(await api.connect(pairing, name.trim() || null));
    } catch (e) {
      setError(messageOf(e));
      setBusy(false);
    }
  };

  const forget = async (server: string) => {
    setError(null);
    try {
      onForgot(await api.forget(server));
    } catch (e) {
      setError(messageOf(e));
    }
  };

  return (
    <Screen icon={Servers} title="servers" onClose={onClose}>
      {/* Trimmed to the one thing that goes wrong. What stood here also
          explained what a pairing string is and where to run the command, and
          the field below has the shape of one in its placeholder -- which
          answers the first question better than a sentence about it does.
          `--host` is the part no placeholder can carry, and per
          docs/desktop.md it is the single commonest reason a paired server
          cannot be reached. */}
      <p className="hint">
        <code>sbxd pair desktop --host …</code> on the machine with the
        sandboxes prints one of these. <code>--host</code> is the address{" "}
        <em>this</em> window should dial — leaving it out is the usual reason a
        paired server cannot be reached.
      </p>

      <label>
        <span>pairing</span>
        <textarea
          ref={field}
          rows={3}
          spellCheck={false}
          placeholder="sbx://host:17671/<token>#<fingerprint>"
          value={pairing}
          onChange={(e) => setPairing(e.target.value)}
          // Enter submits, because the field holds one line that was pasted
          // rather than text anyone types into. Shift-Enter still breaks a
          // line, for a string that arrived wrapped.
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              if (pairing.trim() && !busy) void submit();
            }
          }}
        />
      </label>

      <label>
        <span>name</span>
        <input
          placeholder="defaults to the host"
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
      </label>

      {error && <p className="error">{error}</p>}

      <div className="actions">
        <button className="go" disabled={busy || !pairing.trim()} onClick={() => void submit()}>
          {busy ? "connecting…" : "connect"}
        </button>
      </div>

      {servers.length > 0 && (
        <section className="panel">
          <h3>paired</h3>
          <ul className="paired">
            {servers.map((s) => (
              <li key={s.name}>
                <span className="name">{s.name}</span>
                <span className="address">{s.address}</span>
                {/* Forgetting drops the token this machine holds. The server
                    goes on accepting it until `sbxd revoke` says otherwise,
                    which is the half that matters if it has leaked.

                    The same bin the tree forgets a project with, so one glyph
                    means "drop this from the window" in both places -- and red
                    only under the pointer, because the button is not a warning
                    sitting on screen, it is a warning at the moment you reach
                    for it. */}
                <button
                  className="quiet-icon danger"
                  title={`forget ${s.name} (the server keeps accepting the token until sbxd revoke)`}
                  onClick={() => void forget(s.name)}
                >
                  <Forget aria-label={`forget ${s.name}`} />
                </button>
              </li>
            ))}
          </ul>
        </section>
      )}
    </Screen>
  );
}
