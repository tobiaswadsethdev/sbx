# ai-sandboxer (`sbx`)

[![CI](https://github.com/tobiaswadsethdev/sbx/actions/workflows/ci.yml/badge.svg)](https://github.com/tobiaswadsethdev/sbx/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.89%2B-orange.svg)](https://www.rust-lang.org)

A terminal UI for running several coding agents in parallel, each in its own
[NVIDIA OpenShell](https://github.com/NVIDIA/OpenShell) sandbox.

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

## What it does

* **One sandbox per session.** The agent clones the repository inside it and
  works on `sbx/<name>`; your worktree is never handed over.
* **Credentials the sandbox never sees.** OpenShell providers hold the tokens
  and the gateway substitutes them into outgoing requests.
* **Isolation you can look at.** The policy pane shows the rules being enforced,
  the events feed shows every allow and deny, and both are keys away from
  changing a rule for a running session.
* **Several agents at once, without babysitting.** A session blocked on a
  permission prompt says so in the list, so watching is cheaper than attaching.
* **The parts of your setup that matter, carried in.** Skills are copied into
  each sandbox; MCP servers run on the host, holding their own credentials, and
  are granted per-binary like everything else.
* **Publish from inside.** `sbx publish` pushes the branch and opens a pull
  request on GitHub or Azure DevOps without the token ever reaching your host.

## Quickstart

Linux with systemd and a Docker daemon -- the isolation is kernel-enforced, so
nothing here is portable to macOS. You need [OpenShell](https://github.com/NVIDIA/OpenShell)
0.0.110 with its gateway running, Docker 29.x, tmux, and Rust 1.89 or newer.
[docs/install.md](docs/install.md) walks through all of it, including the
providers that hold your credentials.

```sh
git clone https://github.com/tobiaswadsethdev/sbx && cd sbx
cargo install --path crates/sbx      # -> ~/.cargo/bin/sbx
sbx doctor                           # every prerequisite, and what to do about the missing ones
sbx image build                      # the sandbox image (also happens on first `sbx new`)
sbx new --repo <url> --task "fix the readme typo"
sbx                                  # the TUI
```

`sbx doctor` is the one to run when something looks wrong -- it checks the
gateway, Docker, tmux, lingering, the image and the Claude Code version in it,
plus the providers, skills and MCP servers your config names:

```
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
sbx new --repo <url> --task "what to do"      # sandbox + clone + branch + agent
sbx ls                                        # sessions, reconciled with the gateway
sbx attach <name>                             # attach to the agent; Ctrl-b d to detach
sbx diff <name>                               # what the agent has changed so far
sbx policy <name>                             # the policy the gateway is enforcing
sbx events <name>                             # recent allow/deny decisions
sbx policies                                  # the policy templates shipped in the binary
sbx config                                    # the defaults in force, and where they came from
sbx config --init                             # write a commented ~/.config/sbx/config.toml
sbx publish <name>                            # push the branch and open a pull request
sbx rm <name>                                 # delete session and sandbox
sbx                                           # the TUI: n starts a session, no shell needed
```

`--policy` takes a template name or a path to a YAML file. Three templates ship
in the binary, and `feature-work` is the default:

| Template | Egress |
| --- | --- |
| `readonly-explore` | clone and read; no model API, no push, no PRs |
| `feature-work` | clone, agent, push, open PRs; nothing else reachable |
| `net-open` | `feature-work` plus the npm and PyPI registries |

## Documentation

| | |
| --- | --- |
| [Install](docs/install.md) | prerequisites, the gateway, providers, and `sbx` itself |
| [The TUI](docs/tui.md) | the list, the panes, starting and ending sessions, names and branches |
| [Configuration](docs/configuration.md) | `~/.config/sbx/config.toml`, and which default wins |
| [Policy and events](docs/policy.md) | what is enforced, the audit feed, and acting on a denial |
| [Git hosts](docs/git-hosts.md) | GitHub and Azure DevOps, and how publishing keeps the token away |
| [Skills](docs/skills.md) | carrying your own skills into a sandbox |
| [MCP servers](docs/mcp.md) | servers on the host, and what an MCP server costs you |
| [The sandbox image](docs/sandbox-image.md) | what the image bakes in, and why the agent runs in auto mode |
| [Architecture](docs/architecture.md) | how the pieces fit, for anyone reading the code |
| [The manual loop](docs/manual-loop.md) | the verified setup, run by hand |

## Contributing

Contributions are welcome -- issues, questions and pull requests alike.
[CONTRIBUTING.md](CONTRIBUTING.md) covers the development loop, the test
strategy and what a reviewable change looks like here; the short version is:

```sh
cargo test --workspace               # 388 tests, no gateway or Docker needed
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
moving, and the version is `0.1.0` for a reason.

## License

Apache-2.0. See [LICENSE](LICENSE).
