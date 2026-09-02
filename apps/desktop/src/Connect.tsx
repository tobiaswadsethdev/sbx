// Pairing with a server, from the window rather than from a terminal.
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

import { useEffect, useRef, useState } from "react";

import { api, messageOf, type Paired, type ServerSummary } from "./api";

export function ConnectDialog({
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

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

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
    <div className="scrim" onMouseDown={onClose}>
      <div className="dialog" onMouseDown={(e) => e.stopPropagation()}>
        <header className="dialog-head">
          <h2>Connect to a server</h2>
          <button className="quiet" onClick={onClose}>
            close
          </button>
        </header>

        <p className="hint">
          On the machine with the sandboxes, run <code>sbxd pair desktop --host …</code> and paste
          the line it prints. <code>--host</code> is the address <em>this</em> window should dial,
          and leaving it out is the usual reason a paired server cannot be reached.
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

        {servers.length > 0 && (
          <fieldset>
            <legend>paired</legend>
            <ul className="paired">
              {servers.map((s) => (
                <li key={s.name}>
                  <span className="name">{s.name}</span>
                  <span className="address">{s.address}</span>
                  {/* Forgetting drops the token this machine holds. The server
                      goes on accepting it until `sbxd revoke` says otherwise,
                      which is the half that matters if it has leaked. */}
                  <button className="quiet" onClick={() => void forget(s.name)}>
                    forget
                  </button>
                </li>
              ))}
            </ul>
          </fieldset>
        )}

        <div className="actions">
          <button className="go" disabled={busy || !pairing.trim()} onClick={() => void submit()}>
            {busy ? "connecting…" : "connect"}
          </button>
        </div>
      </div>
    </div>
  );
}
