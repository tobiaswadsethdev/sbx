# ai-sandboxer (`sbx`)

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

## Usage

```sh
sbx doctor                                    # check gateway, docker, tmux, image
sbx image build                               # build the sandbox image (automatic on first use)
sbx new --repo <url> --task "what to do"      # sandbox + clone + branch + agent
sbx ls                                        # sessions, reconciled with the gateway
sbx attach <name>                             # attach to the agent; Ctrl-b d to detach
sbx rm <name>                                 # delete session and sandbox
sbx                                           # the TUI
```

Each agent runs under a tmux session *inside* its own sandbox, so it keeps
working whether or not anything is attached to it.

```
┌ sessions (2) ─────────────────┐┌ preview - readme-fix ──────────────────────────┐
│  add-tests       ready 48s    ││task      Fix the readme typo                   │
│> readme-fix      ready 52s    ││repo      https://github.com/octocat/Hello-World │
│                               ││branch    sbx/readme-fix                        │
│                               ││sandbox   sbx-readme-fix                        │
│                               ││policy    policies/feature-work.yaml            │
│                               ││                                                │
│                               ││status    1 file(s) changed                     │
│                               ││ M README                                       │
└───────────────────────────────┘└────────────────────────────────────────────────┘
 enter attach  j/k move  g/G top/bottom  r refresh  q quit
```

The local cache is disposable: each session's record lives inside its own
sandbox, so deleting `~/.config/sbx/sessions.json` and running `sbx ls`
re-adopts everything still running.

Status: early. See [PLAN.md](PLAN.md) for the increments and
[docs/manual-loop.md](docs/manual-loop.md) for the verified setup.
