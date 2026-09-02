// The Rust side, as functions.
//
// Every type here is generated from the Rust that produces it -- see
// `scripts/gen-bindings.sh` -- so there is no second copy of a message to keep
// in step. `invoke` names have to match `main.rs`'s command names, which is the
// one string pair in this application that a compiler cannot check.

import { invoke } from "@tauri-apps/api/core";

import type { Comment } from "./gen/Comment";
import type { Dir } from "./gen/Dir";
import type { Event } from "./gen/Event";
import type { Against } from "./gen/Against";
import type { FileDiff } from "./gen/FileDiff";
import type { FileText } from "./gen/FileText";
import type { GitOp } from "./gen/GitOp";
import type { Status as GitStatus } from "./gen/Status";
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

/// Hand-written because it is the bridge's own shape rather than a message: see
/// `GitAnswer` in main.rs. Both halves are generated types.
export type GitAnswer = { said: string; status: GitStatus };

export const api = {
  servers: () => invoke<ServerSummary[]>("servers"),
  sessions: (server: string) => invoke<Session[]>("sessions", { server }),
  poll: (server: string, name: string) => invoke<Poll>("poll", { server, name }),
  policy: (server: string, name: string) => invoke<PolicyView>("policy", { server, name }),
  events: (server: string, name: string) => invoke<Event[]>("events", { server, name }),
  diff: (server: string, name: string) => invoke<string>("diff", { server, name }),

  // The working copy, read-only: the agent owns it. One directory at a time,
  // as the tree is expanded -- every listing is an exec into the sandbox.
  files: (server: string, name: string, path: string) =>
    invoke<Dir>("files", { server, name, path }),
  file: (server: string, name: string, path: string) =>
    invoke<FileText>("file", { server, name, path }),

  // Git. Every mutation answers with what git said *and* the status
  // afterwards, re-read rather than assumed: the agent is editing while this
  // runs, so the status after a stage is not the status before it plus one.
  gitStatus: (server: string, name: string) =>
    invoke<GitAnswer>("git_status", { server, name }),
  git: (server: string, name: string, action: GitOp) =>
    invoke<GitAnswer>("git", { server, name, action }),
  gitDiff: (server: string, name: string, path: string, against: Against) =>
    invoke<FileDiff>("git_diff", { server, name, path, against }),

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
