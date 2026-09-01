// The Rust side, as functions.
//
// Every type here is generated from the Rust that produces it -- see
// `scripts/gen-bindings.sh` -- so there is no second copy of a message to keep
// in step. `invoke` names have to match `main.rs`'s command names, which is the
// one string pair in this application that a compiler cannot check.

import { invoke } from "@tauri-apps/api/core";

import type { Event } from "./gen/Event";
import type { Poll } from "./gen/Poll";
import type { Session } from "./gen/Session";
import type { View as PolicyView } from "./gen/View";

export type ServerSummary = { name: string; address: string };

export const api = {
  servers: () => invoke<ServerSummary[]>("servers"),
  sessions: (server: string) => invoke<Session[]>("sessions", { server }),
  poll: (server: string, name: string) => invoke<Poll>("poll", { server, name }),
  policy: (server: string, name: string) => invoke<PolicyView>("policy", { server, name }),
  events: (server: string, name: string) => invoke<Event[]>("events", { server, name }),
  diff: (server: string, name: string) => invoke<string>("diff", { server, name }),
};

/// A command's rejection is a string written for a person, so it is shown
/// rather than interpreted. Anything that is not one is a bug in the bridge,
/// and saying so beats rendering "[object Object]".
export function messageOf(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return `unexpected failure: ${JSON.stringify(e)}`;
}
