// The streaming half, from the window's side.
//
// One connection for the whole window, multiplexed by channel id, which the
// Rust side holds; this is the bookkeeping that turns one event stream back
// into per-pane callbacks.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type { Channel } from "./gen/Channel";
import type { ServerFrame } from "./gen/ServerFrame";

/// The id the Rust side uses for a `Closed` about the whole connection rather
/// than one channel. Kept in step with `ALL_CHANNELS` in `main.rs`.
const ALL_CHANNELS = 4294967295;

type Handler = (frame: ServerFrame) => void;

const handlers = new Map<number, Handler>();
let listening: Promise<UnlistenFn> | null = null;
let nextId = 1;

/// Ids are never reused within a run.
///
/// A pane that closes and one that opens can otherwise collide: the close and
/// the open race, and a late frame for the old channel lands in the new pane.
/// Counting up costs nothing and removes the question.
export function nextChannelId(): number {
  return nextId++;
}

function ensureListening() {
  listening ??= listen<ServerFrame>("sbx://frame", (event) => {
    const frame = event.payload;
    if (frame.id === ALL_CHANNELS) {
      // The connection went, so every channel went with it.
      for (const handler of handlers.values()) handler(frame);
      handlers.clear();
      return;
    }
    handlers.get(frame.id)?.(frame);
  }).catch((e) => {
    // Not cached as a rejection: a failure here is a capability or a
    // permission, and a retry after a fix should be able to succeed rather
    // than being told about the first attempt for ever.
    listening = null;
    throw e;
  });
  return listening;
}

/// Open a channel and route its frames to `onFrame` until `close` is called.
export async function open(
  server: string,
  id: number,
  channel: Channel,
  onFrame: Handler,
): Promise<void> {
  // Registered before the first `await`, so that a `close` arriving while this
  // is still in flight has something to remove. React's StrictMode mounts every
  // effect twice in development, which makes open-then-immediately-close the
  // *normal* order rather than a rare one -- and a handler registered after its
  // own close would sit in the map for the life of the window, taking frames
  // for a pane that no longer exists.
  handlers.set(id, onFrame);
  const stillWanted = () => handlers.get(id) === onFrame;

  try {
    await ensureListening();
    if (!stillWanted()) return;

    await invoke("watch", { server, id, channel });

    // Closed while the open was in flight. The server has a channel nobody is
    // listening to now, so it is told -- otherwise it streams a terminal into
    // a queue that is never drained until the connection goes.
    if (!stillWanted()) {
      void invoke("unwatch", { id }).catch(() => {});
    }
  } catch (e) {
    if (stillWanted()) handlers.delete(id);
    throw e;
  }
}

export async function close(id: number): Promise<void> {
  handlers.delete(id);
  // Best effort: a channel the server has already closed is not an error worth
  // showing, and the pane is going away regardless.
  await invoke("unwatch", { id }).catch(() => {});
}

export const terminal = {
  input: (id: number, data: string) => invoke("terminal_input", { id, data }),
  resize: (id: number, cols: number, rows: number) =>
    invoke("terminal_resize", { id, cols, rows }),
};

/// Base64, matching `sbx_proto::stream::bytes` at the other end.
///
/// Terminal traffic is raw bytes: a pty read lands wherever it lands, so a
/// multi-byte character can be split across two frames. Decoding to a
/// `Uint8Array` and letting xterm reassemble is the only way that survives --
/// decoding to a string would replace the halves with U+FFFD.
export function decodeBytes(data: string): Uint8Array {
  const binary = atob(data);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
  return out;
}

export function encodeBytes(raw: Uint8Array): string {
  let binary = "";
  for (const byte of raw) binary += String.fromCharCode(byte);
  return btoa(binary);
}
