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

## Prerequisites

Linux with systemd and a Docker daemon. Verified on Arch on WSL2; nothing here
is portable to macOS, because the isolation is kernel-enforced.

| | |
| --- | --- |
| Rust | 1.88 or newer (edition 2024, let-chains) |
| [OpenShell](https://github.com/NVIDIA/OpenShell) | 0.0.110 -- CLI, gateway and sandbox helper |
| Docker | server 29.x, reachable by your user |
| tmux | on the host, for `sbx attach` |

`sbx doctor` checks every one of them, plus the sandbox image and whether
systemd lingering is enabled, and says what to do about whatever is missing:

```
[  ok  ] openshell    openshell 0.0.110
[  ok  ] gateway      https://127.0.0.1:17670 0.0.110 (authenticated)
[  ok  ] docker       server 29.6.0
[  ok  ] tmux         tmux 3.6b
[  ok  ] linger       enabled
[  ok  ] image        sbx-base:latest built
```

## Install

**OpenShell.** The official `install.sh` supports dpkg and rpm only; on
anything else install the release tarballs into `~/.local/bin`, which needs no
root. The gateway runs as a systemd *user* service:

```sh
systemctl --user enable --now openshell-gateway
openshell gateway add https://127.0.0.1:17670 --local --name openshell
openshell status                       # -> Connected, Authenticated (mTLS)
sudo loginctl enable-linger $USER      # WSL: or the gateway dies with your shell
```

The tarball names, checksums and the unit file are in
[docs/manual-loop.md](docs/manual-loop.md) and
[docs/openshell-gateway.service](docs/openshell-gateway.service).

**Providers.** One per credential the agents need. The profiles are in
`providers/`; `--credential KEY` reads the value from the environment at create
time and stores it in gateway state, so the shell that ran it can be closed:

```sh
openshell provider profile import --file providers/claude-code-oauth.yaml
read -rs -p "paste token: " CLAUDE_CODE_OAUTH_TOKEN   # `claude setup-token`
export CLAUDE_CODE_OAUTH_TOKEN
openshell provider create --name claude-oauth \
        --type claude-code-oauth --credential CLAUDE_CODE_OAUTH_TOKEN
```

`read` needs a TTY, so that has to be a real terminal. For Azure DevOps, do the
same with `providers/azure-devops-pat.yaml` (see [Git hosts](#git-hosts)).

**`sbx` itself.** The policy templates and the whole image recipe -- Dockerfile,
status hook, Claude settings -- are compiled into the binary, so it needs
nothing from this tree at runtime except the provider profiles above, which the
`openshell` CLI reads directly:

```sh
cargo install --path crates/sbx      # -> ~/.cargo/bin/sbx
sbx image build                      # also happens on first `sbx new`
sbx doctor
```

## Building and testing

```sh
cargo build
cargo test --workspace               # 171 tests, no gateway needed
cargo run -- doctor                  # the CLI, from the tree
cargo run                            # the TUI, from the tree
```

The unit tests are hermetic on purpose: pane classification runs against real
captures in `crates/sbx/tests/panes/`, the TUI's key handling drives `App`
directly with synthetic key events, and rendering is checked through the pure
helpers that build the lines rather than a terminal. So the whole suite runs
without OpenShell, Docker or a network.

What that cannot cover is the gateway contract, which lives in ignored tests
and needs a live gateway and Docker:

```sh
cargo test -p openshell-client -- --ignored --test-threads=1
```

They create and delete real sandboxes labelled `sbx.test`, one at a time.

## Usage

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
sbx publish <name>                            # push the branch and open a pull request
sbx rm <name>                                 # delete session and sandbox
sbx                                           # the TUI
```

`--policy` takes a template name or a path to a YAML file. Three templates ship
in the binary, and `feature-work` is the default:

| Template | Egress |
| --- | --- |
| `readonly-explore` | clone and read; no model API, no push, no PRs |
| `feature-work` | clone, agent, push, open PRs; nothing else reachable |
| `net-open` | `feature-work` plus the npm and PyPI registries |

## Git hosts

GitHub and Azure DevOps, detected from the repo URL rather than configured.
`sbx publish` pushes the work branch and opens a pull request from *inside* the
sandbox, so the host never holds the credential:

```sh
sbx new --repo 'https://dev.azure.com/org/project/_git/repo' \
        --task "..." --provider azure-pat --provider claude-oauth
sbx publish <name>          # -> https://dev.azure.com/org/project/_git/repo/pullrequest/10
```

Credentials come from OpenShell providers, and the sandbox never sees them: the
provider sets an environment variable holding a *placeholder*, and the gateway
substitutes the real token into the outgoing request. An Azure DevOps PAT is
scoped to one organisation, so mint one per org and attach the right one per
session:

```sh
export AZURE_DEVOPS_PAT='...'   # Code (Read & Write)
openshell provider profile import --file providers/azure-devops-pat.yaml
openshell provider create --name azure-pat --type azure-devops-pat \
        --credential AZURE_DEVOPS_PAT     # env lookup; the token stays out of your shell history
```

Pull requests on Azure DevOps are created with a plain REST call rather than the
Azure CLI, so the image stays as it is. `readonly-explore` reaches neither
`git-receive-pack` nor `_apis`, so a session under it can read a repository and
provably cannot publish to it.

Each agent runs under a tmux session *inside* its own sandbox, so it keeps
working whether or not anything is attached to it.

```
┌ sessions (2, 1 waiting) ────────────────┐┏ diff - readme-fix [22/61] ━━━━━━━━━━━━━━━━━━┓
│  add-tests    waiting  clean       48s  │┃── committed, vs origin/main                 ┃
│> readme-fix   running  +12/-3 ?    52s  │┃diff --git a/README b/README                 ┃
│                                         │┃@@ -1,4 +1,4 @@                              ┃
│                                         │┃-Hello Wrold!                                ┃
│                                         │┃+Hello World!                                ┃
│                                         │┃── uncommitted                               ┃
│                                         │┃...                                          ┃
│                                         │┃── untracked                                 ┃
│                                         │┃tests/test_readme.py                         ┃
└─────────────────────────────────────────┘┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛
 j/k scroll  pgup/pgdn page  h pane  tab view  enter attach  q quit
```

The state column is what the *agent* is doing, not just whether the sandbox is
up. A session blocked on a permission prompt shows `waiting` as a filled badge
and is counted in the title, so you can see it without scrolling to it -- that
notification is the reason to run several sessions at once. It comes from
scraping the agent's screen as well as from hooks baked into the image, because
Claude Code fires no hook for a permission prompt or an interrupt; the preview
pane says which source decided.

`Tab` cycles the right pane through preview, diff, policy and events
(`Shift-Tab` goes back), remembered per session. `h`/`l` move focus between the
panes, and the movement keys follow it: `j`/`k` walk the session list on the
left and scroll on the right. The `+12/-3` column counts lines changed against
the branch the session started from, and `?` marks untracked files. Every pane
refetches on a timer, so a diff you are reading keeps up with the agent editing
underneath it.

## Policy

The isolation is the point, so it is visible rather than buried in a YAML file.
The **policy** pane shows the rules the gateway is actually enforcing, per
binary, and the **events** pane is the allow/deny feed behind them:

```
┌ events (UTC) - add-tests ───────────────────────────────────────────────────┐
│11:15:02  allow  GET github.com:443/octocat/Hello-World.git/info/refs  [git] │
│11:15:02  DENY   /usr/bin/curl(93) -> pastebin.com:443                       │
│             endpoint pastebin.com:443 is not allowed by any policy          │
└─────────────────────────────────────────────────────────────────────────────┘
```

In the policy pane, `w` widens egress to the package registries and `t`
tightens it back, without restarting the agent -- for the task that turns out
to need a dependency installed. Only the network section: the filesystem and
process sections are fixed when the sandbox is created, and the gateway will
accept a change to them, report it as effective, and never enforce it, so the
pane labels them and declines to offer it.

The local cache is disposable: each session's record lives inside its own
sandbox, so deleting `~/.config/sbx/sessions.json` and running `sbx ls`
re-adopts everything still running.

Status: early. See [PLAN.md](PLAN.md) for the increments and
[docs/manual-loop.md](docs/manual-loop.md) for the verified setup.
