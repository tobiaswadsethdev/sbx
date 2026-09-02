// The Rust side, as functions.
//
// Every type here is generated from the Rust that produces it -- see
// `scripts/gen-bindings.sh` -- so there is no second copy of a message to keep
// in step. `invoke` names have to match `main.rs`'s command names, which is the
// one string pair in this application that a compiler cannot check.

import { invoke } from "@tauri-apps/api/core";

import type { Comment } from "./gen/Comment";
import type { Event } from "./gen/Event";
import type { NewComment } from "./gen/NewComment";
import type { Picked } from "./gen/Picked";
import type { Listing } from "./gen/Listing";
import type { NewOptions } from "./gen/NewOptions";
import type { NewProject } from "./gen/NewProject";
import type { Project } from "./gen/Project";
import type { NewSession } from "./gen/NewSession";
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

  // Shells beside the agent, in the same sandbox under the same policy. What
  // exists is asked of the sandbox rather than remembered here, so a shell
  // survives this window closing.
  shells: (server: string, name: string) => invoke<string[]>("shells", { server, name }),
  newShell: (server: string, name: string) => invoke<string[]>("new_shell", { server, name }),
  killShell: (server: string, name: string, tmux: string) =>
    invoke<string[]>("kill_shell", { server, name, tmux }),

  // The review. Kept on the server, per session, so an unsent one survives the
  // window closing -- see `sbx_core::comments`.
  comments: (server: string, name: string) => invoke<Comment[]>("comments", { server, name }),
  comment: (server: string, name: string, comment: NewComment) =>
    invoke<Comment[]>("comment", { server, name, comment }),
  uncomment: (server: string, name: string, id: number) =>
    invoke<Comment[]>("uncomment", { server, name, id }),
  sendComments: (server: string, name: string) =>
    invoke<string>("send_comments", { server, name }),

  // Projects: the repositories someone has decided to work on, which is what
  // the tree groups worktrees under.
  projects: (server: string) => invoke<Project[]>("projects", { server }),
  newProject: (server: string, project: NewProject) =>
    invoke<Project[]>("new_project", { server, project }),
  forgetProject: (server: string, name: string) =>
    invoke<Project[]>("forget_project", { server, name }),

  // The create flow. `repos` and `inspect` answer about the *server's* disk:
  // a checkout is only a way of naming a remote, but which checkouts exist is a
  // fact about the machine that will do the cloning.
  repos: (server: string) => invoke<Listing>("repos", { server }),
  inspect: (server: string, path: string, branch: string | null) =>
    invoke<Picked>("inspect", { server, path, branch }),
  newOptions: (server: string) => invoke<NewOptions>("new_options", { server }),
  create: (server: string, session: NewSession) => invoke<string>("create", { server, session }),
};

/// A command's rejection is a string written for a person, so it is shown
/// rather than interpreted. Anything that is not one is a bug in the bridge,
/// and saying so beats rendering "[object Object]".
export function messageOf(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return `unexpected failure: ${JSON.stringify(e)}`;
}
