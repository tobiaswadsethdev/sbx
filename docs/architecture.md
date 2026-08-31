# Architecture

A tour of the code, for anyone about to change some of it. [PLAN.md](../PLAN.md)
has the decisions and the increments that got here; this is the map.

## The shape of it

```
                +-------------------------+
                |        sbx (TUI)        |   ratatui + crossterm
                | list | agent  | diff    |
                |      | policy | events  |
                +-----------+-------------+
                            |
        +-------------------+--------------------+
        |                   |                    |
   SessionStore        openshell-client       tmux
   (~/.config/sbx/     (CLI subprocess,      (inside each sandbox,
    sessions.json)      one trait)            capture-pane, attach)
                            |
                    openshell gateway (docker driver)
                            |
        +-------------------+--------------------+
        |                   |                    |
    sandbox A           sandbox B            sandbox C
   clone+agent         clone+agent          clone+agent
```

Two crates:

| | |
| --- | --- |
| `crates/openshell-client` | everything the rest of the tool knows about OpenShell, behind one trait. OpenShell is a fast-moving `0.0.x` project, so version churn lands in one file -- and the trait is what lets the gRPC API replace the subprocess later without touching callers |
| `crates/sbx` | the CLI and the TUI, and all the logic between them |

## Where things live

The module docs at the top of each file are the real documentation; this is the
index into them.

| Module | What it owns |
| --- | --- |
| `main.rs` | the clap CLI, and dispatch into `ops` |
| `ops.rs` | the operations both the CLI and the TUI need, so neither reimplements the other |
| `session.rs` | what a session *is*: identity, the derived branch and sandbox names, and the metadata record written inside the sandbox |
| `store.rs` | the local cache and its reconciliation against the gateway; every write is locked |
| `seed.rs` | the detached script that clones, cuts the branch, writes the record and starts the agent |
| `status.rs` | what the agent is doing, from hooks and from its screen |
| `policy.rs` | the templates, the mid-run widen/tighten, and the policy pane's body |
| `endpoints.rs` | the global allow and block lists applied to every new session |
| `events.rs` | the allow/deny feed, merged and kept on disk per session |
| `forge.rs` | which git host a session works against, derived from the repo URL |
| `publish.rs` | push and open a pull request, both from inside the sandbox |
| `image.rs` | the sandbox image, with its whole build context embedded in the binary |
| `toolchain.rs` | the toolchains, their image variants, and the registry each one opens |
| `skills.rs` | packing host skills into a session |
| `mcp.rs` | MCP servers on the host, and the endpoints granted for them |
| `repos.rs` | the git repositories on your own disk -- the only module that reads the host |
| `config.rs` | `~/.config/sbx/config.toml`, and which default wins |
| `doctor.rs` | the preflight checks, each carrying its fix |
| `update.rs` | fetching, verifying and replacing the `sbx` binary itself |
| `ansi.rs` | one tokenizer for captured screens, shared by the pane that shows them and the matcher that reads them |
| `pane.rs` | the markup the text panes share, so styling stays in one place |
| `tui/mod.rs` | `App`: state and key handling |
| `tui/ui.rs` | rendering; almost pure, and tested through the helpers that build the lines |
| `tui/worker.rs` | the background worker that owns every gateway call |
| `tui/create.rs` | the repo picker and the create form, as pure state machines |
| `tui/attach.rs` | handing the terminal to the agent and taking it back |

## Two rules worth knowing before you change anything

**The render thread does no I/O.** Every gateway call is a subprocess round trip
costing hundreds of milliseconds. `tui/worker.rs` owns all of them; the UI sends
`Request`s and drains `Update`s. A feature that needs to ask the gateway
something adds a request and an update, not a call in the key handler.

**The sandbox is the source of truth.** Seeding writes
`/sandbox/.sbx/meta.json`, so a session describes itself and survives losing the
local cache. Labels carry identity only -- the gateway restricts label values to
Kubernetes rules, at most 63 characters of `[A-Za-z0-9._-]`, which cannot hold a
repo URL or a branch name with a `/` in it.

The local cache is disposable: each session's record lives inside its own
sandbox, so deleting `~/.config/sbx/sessions.json` and running `sbx ls`
re-adopts everything still running. Every write to it is a locked
read-modify-write, because more than one writer is the normal case -- a TUI
reconciling the list every second while `sbx new` in another terminal walks a
session from `seeding` to `ready`, which on a large repository takes minutes.

**Seeding runs inside the sandbox, detached from the command that asks for it.**
`sbx new` writes a script into the sandbox, starts it with `setsid`, and then
only *watches* it: the clone, the work branch, the metadata record and the agent
all happen in there, and they finish whether or not the tool that started them is
still running. Killing `sbx new` with `SIGKILL` two seconds into a clone leaves a
session that comes up complete on its own.

The seeder reports each step into `/sandbox/.sbx/seed.state` and its output to
`seed.log`, which is what makes the difference between the three things that used
to look identical from outside: still cloning, finished while nobody was looking,
and stopped. The first refresh of any `sbx` command reads that file for a session
whose record still says `creating` or `seeding` and catches the record up --
`seed-kill: seeding -> ready (seeding finished)` -- or marks it failed with the
reason git gave. A seeder still running is left alone, however long it takes.

## Tests

The suite is hermetic on purpose, so a contributor with no gateway can still
change almost anything:

* pane classification runs against real captures in `crates/sbx/tests/panes/`,
  included at compile time by `status.rs`;
* the TUI's key handling drives `App` directly with synthetic key events;
* rendering is checked through the pure helpers that build the lines, rather
  than through a terminal.

What that cannot cover is the gateway contract, which lives in ignored tests in
`crates/openshell-client/tests/live.rs` and needs a live gateway and Docker.
They create and delete real sandboxes labelled `sbx.test`, one at a time:

```sh
cargo test -p openshell-client -- --ignored --test-threads=1
```

[CONTRIBUTING.md](../CONTRIBUTING.md) has the rest of the development loop.

---

[← Documentation](README.md) · [README](../README.md)
