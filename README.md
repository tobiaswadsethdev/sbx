# sbx

[![CI](https://github.com/tobiaswadsethdev/sbx/actions/workflows/ci.yml/badge.svg)](https://github.com/tobiaswadsethdev/sbx/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.89%2B-orange.svg)](https://www.rust-lang.org)

A terminal UI and a desktop workspace for running several coding agents in
parallel, each in its own [NVIDIA OpenShell](https://github.com/NVIDIA/OpenShell)
sandbox.

Claude Squad's workflow, with real isolation underneath: kernel-enforced
filesystem, network and process policy per session, credentials injected at
runtime instead of sitting on disk, and an audit trail of every allow/deny.

Network policy binds endpoints to **binaries**, not just hosts, so a session can
be configured such that:

```
git clone https://github.com/octocat/Hello-World.git   -> SUCCEEDS
curl https://github.com                                 -> DENIED
```

```
   sessions 2                           1 waiting     agent · diff · policy · events          readme-fix

      1. add-tests                      waiting ●     ── committed, vs origin/main
         sbx/add-tests                   clean 48s    diff --git a/README b/README
                                                      @@ -1,4 +1,4 @@
   ▐ 2. readme-fix                      running ●▌    -Hello Wrold!
   ▐    sbx/readme-fix               +12/-3 ? 52s▌    +Hello World!
                                                      ── uncommitted
                                                      ...
   session readme-fix                                 ── untracked

   task      fix the readme typo                      tests/test_readme.py
   repo      https://github.com/you/sbx.git
   branch    sbx/readme-fix
   sandbox   sbx-readme-fix
   policy    feature-work
   agent     claude
   providers claude-oauth
   agent at  running  Edit  (screen)

   j/k move · 1-9 jump · n new  │  enter open · a attach · P publish · D destroy  │  tab view · q quit
```

The desktop application is the same thing as a workspace: projects containing
worktrees, the agent's terminal and extra shells beside it, the working copy in
a file tree, diffs in an editor with comments that go back to the agent, and git
on the right.

```
  sbx  127.0.0.1:17671  3 worktrees in 1 project   [new project]
  +--------------+-------------------------------------+----------------------+
  | sbx          | agent | shell-1 x | main.rs | diff ~ | files git events ... |
  |   readme-fix |                                     |  branch sbx/readme   |
  |   add-tests  |   1  -Hello Wrold!                  |  fetch pull push     |
  | octocat/demo |   1  +Hello World!                  |  CHANGES 2           |
  |   spike      |                                     |  M README            |
  +--------------+-------------------------------------+----------------------+
```

## What it does

- **One sandbox per session.** The agent clones the repository inside it and
  works on `sbx/<name>`; your worktree is never handed over.
- **Credentials the sandbox never sees.** OpenShell providers hold the tokens
  and the gateway substitutes them into outgoing requests.
- **Isolation you can look at.** The policy view shows the rules being enforced
  and the events feed shows every allow and deny -- in both front ends, one key
  or one click away, and a rule can be widened for a running session from there.
  This is the part an ADE built on git worktrees has no equivalent for.
- **Several agents at once, without babysitting.** A session blocked on a
  permission prompt says so in the list, so watching is cheaper than attaching.
- **A toolchain when the task needs one.** `--toolchain dotnet` runs the session
  on an image variant carrying the SDK, and opens nuget for the SDK's binary and
  nothing else. The create form ticks it from what the repository contains.
- **The parts of your setup that matter, carried in.** Skills are copied into
  each sandbox -- pushed from the machine you are sitting at, so editing one
  reaches the next session even when the sessions are somewhere else. MCP
  servers run on the host, holding their own credentials, and are granted
  per-binary like everything else; `sbxd` can own their containers and their
  secrets, with a screen that says what each one is doing.
- **Publish from inside.** `sbx publish` pushes the branch and opens a pull
  request on GitHub or Azure DevOps without the token ever reaching your host.
- **An inbox, and the loop back to it.** What GitHub, Azure DevOps and Jira say
  is assigned to you, read by the server; one button turns a ticket into a
  session with the task, the name and the branch already right, and publishing
  comments the pull request back onto the ticket and moves it.
- **Two front ends over one server.** The same sessions from a terminal or from
  a window, and the window can be on a different machine from the sandboxes --
  see [docs/server.md](docs/server.md).
- **A worktree, when a sandbox is the wrong tool.** `--worktree` starts the
  session as a `git worktree` on the server instead: seconds rather than
  minutes, the machine's own toolchains, and **no isolation whatsoever** -- so
  it is labelled that way in every list, the policy and events panes say so, and
  it is never the default. [docs/worktrees.md](docs/worktrees.md) is the trade.

## Quickstart

Linux with systemd and a Docker daemon -- the isolation is kernel-enforced, so
nothing here is portable to macOS. You need [OpenShell](https://github.com/NVIDIA/OpenShell)
0.0.110 with its gateway running, Docker 29.x and tmux -- plus Rust 1.89 or
newer, but only to build it yourself. [docs/install.md](docs/install.md) walks
through all of it, including the providers that hold your credentials.

```sh
curl -fsSL https://raw.githubusercontent.com/tobiaswadsethdev/sbx/main/install.sh | sh

sbx doctor                           # every prerequisite, and what to do about the missing ones
sbx image build                      # the sandbox image (also happens on first `sbx new`)
sbx new --repo <url> --task "fix the readme typo"
sbx                                  # the TUI
```

The script needs no checkout and no Rust toolchain: it fetches the newest
release for your machine, checks it against the published `SHA256SUMS`, and
puts the binary in `~/.local/bin` -- then runs `sbx doctor` to say what is still
missing. It falls back to building with `cargo` when no release matches your
machine, and `--bin-dir`, `--version` and `--from-source` are there when you
want to decide those yourself. From a checkout, `cargo install --path
crates/sbx` does the same job.

`sbx update` later fetches, verifies and replaces the binary the same way.
Nothing updates itself in the background; `sbx doctor` is what mentions that a
newer release is out.

**The window is installed separately, and can be on another machine.** On Linux
it is built from the tree; on Windows it is an installer from the [releases
page](https://github.com/tobiaswadsethdev/sbx/releases) and is all that side
needs -- it pairs with a server from its own dialog, so there is no `sbx` to
install there. Both are [docs/install.md](docs/install.md#the-desktop-application).

`sbx doctor` is the one to run when something looks wrong -- it checks the
gateway, Docker, tmux, lingering, the image and the Claude Code version in it,
plus the providers, skills and MCP servers your config names and the toolchain
variants you have built:

```
[  ok  ] version      sbx 0.2.0, newest
[  ok  ] openshell    openshell 0.0.110
[  ok  ] gateway      https://127.0.0.1:17670 0.0.110 (authenticated)
[  ok  ] docker       server 29.6.0
[  ok  ] tmux         tmux 3.6b
[  ok  ] linger       enabled
[  ok  ] image        sbx-base:latest built, claude 2.1.246
```

## Commands

```sh
sbx doctor                                    # check gateway, docker, tmux, image
sbx image build                               # build the sandbox image (automatic on first use)
sbx image build --toolchain dotnet,rust       # ... plus toolchains, as their own image variant
sbx new --repo <url> --task "what to do"      # sandbox + clone + branch + agent
sbx new --worktree --repo <path> --task "..."  # ... or a git worktree here, with no isolation
sbx ls                                        # sessions, reconciled with the gateway
sbx attach <name>                             # attach to the agent; Ctrl-b d to detach
sbx diff <name>                               # what the agent has changed so far
sbx policy <name>                             # the policy the gateway is enforcing
sbx events <name>                             # recent allow/deny decisions
sbx tasks                                     # the task inbox: what is assigned to you
sbx policies                                  # the policy templates shipped in the binary
sbx toolchains                                # the toolchains a sandbox image can be built with
sbx config                                    # the defaults in force, and where they came from
sbx config --init                             # write a commented ~/.config/sbx/config.toml
sbx publish <name>                            # push the branch and open a pull request
sbx update                                    # fetch and verify the newest release of sbx itself
sbx rm <name>                                 # delete session and sandbox
sbx                                           # the TUI: n starts a session, no shell needed

sbxd serve                                    # serve this machine's sessions over one TLS port
sbxd pair <client>                            # a string that pairs a client with this machine
sbxd mcp                                      # the MCP catalog, and what each managed one is doing
printf %s "$TOKEN" | sbxd secret <NAME>       # store a secret a managed MCP server needs
sbxd skills                                   # the skills a client has uploaded here
sbx connect <string>                          # pair with a server
sbx --server=<name> ls                        # ... and ask it instead of the local gateway
sbx watch <name> --server=<name>              # follow a session's events and state as they happen
```

`--policy` takes a template name or a path to a YAML file. Three templates ship
in the binary, and `feature-work` is the default:

| Template           | Egress                                               |
| ------------------ | ---------------------------------------------------- |
| `readonly-explore` | clone and read; no model API, no push, no PRs        |
| `feature-work`     | clone, agent, push, open PRs; nothing else reachable |
| `net-open`         | `feature-work` plus the npm and PyPI registries      |

A package registry is otherwise a _toolchain's_ to open, not a template's:
`--toolchain rust` grants crates.io to cargo, in that session, and to nothing
else. See [docs/toolchains.md](docs/toolchains.md).

## Documentation

|                                            |                                                                       |
| ------------------------------------------ | --------------------------------------------------------------------- |
| [Install](docs/install.md)                 | prerequisites, the gateway, providers, `sbx`, and the window on Linux and Windows |
| [The TUI](docs/tui.md)                     | the list, the panes, starting and ending sessions, names and branches |
| [The desktop app](docs/desktop.md)         | projects and worktrees, files, git, the editor, and the review        |
| [The server](docs/server.md)               | `sbxd`, pairing a client on another machine, WSL, what a token is worth |
| [Configuration](docs/configuration.md)     | `~/.config/sbx/config.toml`, and which default wins                   |
| [Policy and events](docs/policy.md)        | what is enforced, the audit feed, and acting on a denial              |
| [Worktree sessions](docs/worktrees.md)     | sessions with no sandbox: what they buy, and everything they give up  |
| [The task inbox](docs/inbox.md)            | tickets in, sessions out, and the publish that writes back            |
| [Git hosts](docs/git-hosts.md)             | GitHub and Azure DevOps, and how publishing keeps the token away      |
| [Toolchains](docs/toolchains.md)           | node, .NET and Rust in a sandbox, and the registry each one may reach |
| [Skills](docs/skills.md)                   | carrying your own skills into a sandbox                               |
| [MCP servers](docs/mcp.md)                 | servers sbxd runs or you do, their secrets, and what one costs you    |
| [The sandbox image](docs/sandbox-image.md) | what the image bakes in, and why the agent runs in auto mode          |
| [Architecture](docs/architecture.md)       | how the pieces fit, for anyone reading the code                       |
| [The manual loop](docs/manual-loop.md)     | the verified setup, run by hand                                       |

## Contributing

Contributions are welcome -- issues, questions and pull requests alike.
[CONTRIBUTING.md](CONTRIBUTING.md) covers the development loop, the test
strategy and what a reviewable change looks like here; the short version is:

```sh
cargo test --workspace               # 539 tests, no gateway or Docker needed
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

[docs/architecture.md](docs/architecture.md) is a tour of the crates and
modules, and worth ten minutes before a first change.

By taking part you agree to the [Code of Conduct](CODE_OF_CONDUCT.md). Security
reports have their own route: [SECURITY.md](SECURITY.md).

## Status

Early, and honest about it. [PLAN.md](PLAN.md) is the record of what has been
built increment by increment and what is still on the list; interfaces are still
moving, and the version is `0.2.0` for a reason -- the minor bump is toolchains
being a thing a session now has, not a promise that anything has settled.

## License

Apache-2.0. See [LICENSE](LICENSE).
