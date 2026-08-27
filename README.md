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
| Rust | 1.89 or newer (edition 2024, let-chains, `File::lock`) |
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
[  ok  ] image        sbx-base:latest built, claude 2.1.246
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
cargo test --workspace               # 234 tests, no gateway needed
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
sbx                                           # the TUI: n starts a session, no shell needed
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

The image bakes a `settings.json` for it, because a fresh sandbox has a fresh
`HOME` and an agent that has to be configured on arrival is an agent that stops
to ask:

| | |
| --- | --- |
| `model` | `opus[1m]` -- an alias, so it follows the newest Opus and keeps the million-token context |
| `permissions.defaultMode` | `auto`, so the agent handles its own permission prompts |
| `env` | the auto-updater, non-essential traffic and the plugin marketplace, all off |
| `hooks` | the status reporter, so the state column has something to read |

**Auto mode** is Claude Code's own middle setting: it judges each tool call and
executes what it considers safe, rather than stopping for every edit
(`acceptEdits` stops for everything that is not one) or not asking at all
(`bypassPermissions`). Claude Code's own advice is to use it "only in isolated
environments", which is the one thing sbx can actually promise -- and it is the
whole reason to run several agents at once, since an agent that stops on the
first edit is an agent you are still babysitting. `Shift+Tab` inside a session
changes it, and `/model` changes the model, for that session.

The three environment variables are all there because the sandbox *denies* the
traffic behind them, and a denial with nothing worth investigating behind it is
noise in the events pane. With them set, a session that clones, edits and answers
produces a feed with no denials in it at all.

`sbx image build` installs the newest Claude Code release rather than whatever
the community base image happens to have frozen -- it shipped 2.1.143 while
2.1.246 was current, and an agent cannot upgrade itself from inside a sandbox
with no writable install path and no route to the download service. The version
is resolved on the host and passed in as a build arg, so a rebuild really does
fetch what is newest instead of being answered from a cached layer, and the
download is checked against the release manifest's SHA-256.
`--build-arg CLAUDE_VERSION=2.1.246` pins a specific one. `sbx doctor` reports
what the built image carries and warns when a newer release is out.

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

The left column is what a session *is*: the list, and under it the facts about
whichever one the cursor is on. They sit there rather than in a pane of their own
so they stay on screen whatever the right-hand side is showing. Each fact is one
row, cut to the pane rather than wrapped. Nothing is hidden by that: a task cut
short here is in the agent's screen in full, since the prompt it was given is the
first thing in its transcript.

A session takes two rows -- what it is, then where it has got to -- because those
are two different questions and answering both on one line left room for neither.
The rows are numbered, and `1`-`9` jump straight to one. The right-hand pane's
views are tabs in its heading, so the pane keeps every row of its height for
content.

The selected session is a filled light block -- the `▐ ▌` above stands in for it
here -- with its text darkened to suit: black for the name and state, grey for
the number, branch and age. The diff stat keeps its green and red, and the state
dot its colour, because those read on white in either kind of theme. `waiting`
keeps its magenta there too; everywhere else it is a filled magenta badge, which
cannot survive inside another fill.

Nothing sits in a corner of the terminal: the whole interface is inset, the
columns are held apart, and every heading has a blank row under it. The footer's
hints shed their descriptions when the window is too narrow for them -- `j/k`
rather than `j/k move` -- because a hint line clipped mid-word reads as broken and
the keys are the part worth keeping.

There are no boxes. In a layout this dense they cost more than they earn -- four
of them, drawn around content that is mostly rules already -- and what a border
was really carrying was which pane the movement keys belong to. The heading
carries that instead, in bold, where the eye already is. The create flow's picker
and form keep their edge, because a modal is drawn over whatever was underneath
it and its border is the only thing saying where it stops.

The state column is what the *agent* is doing, not just whether the sandbox is
up. A session blocked on a permission prompt shows `waiting` as a filled badge
and is counted in the title, so you can see it without scrolling to it -- that
notification is the reason to run several sessions at once. It comes from
scraping the agent's screen as well as from hooks baked into the image, because
Claude Code fires no hook for a permission prompt or an interrupt; the `agent at`
line says which source decided.

`Tab` cycles the right pane through the agent's screen, the diff, the policy and
the events feed (`Shift-Tab` goes back), remembered per session. The agent's
screen is where it starts, because it answers the question the list raises: the
state column says an agent is waiting, and this says what for. `Enter` attaches
to the agent, `P` publishes and `D` destroys -- the two that are hard to undo are the two on capitals, and both ask
first. `Shift-↑`/`Shift-↓` scroll the right-hand pane from either side, and
`PageUp`/`PageDown` page it; `h`/`l` move focus between the panes, after which
`j`/`k` scroll rather than walk the list. The footer always says what the keys are
here, because they change with the focus. The `+12/-3` column counts lines changed against
the branch the session started from, and `?` marks untracked files.

Everything refetches on a timer, and the timers are short: a change inside a
sandbox is on screen in **under 600ms** for the session you are looking at, and
within two seconds for the rest. That is affordable because the reads are cheap --
`sandbox list` is 20ms, a full poll of one session is 56ms, `git status` on a ten
thousand file repository is 65ms -- so the whole interface costs a fraction of a
percent of a core. The selected session is polled hardest, since its state, its
stat and its screen all come out of that one read; the floor between polls caps
the rate at five a second across every session, which keeps a long list from
turning into a stream of execs.

### Starting a session

`n` opens a picker over the git repositories on your disk -- type to filter,
enter to choose -- and then a form for everything `sbx new` takes:

```
┌ pick a repo (15) ────────────────────────────────────────────────────────────┐
│ > sbx                                                                        │
│> ~/dev/sbx                              main                                 │
│  ~/dev/sbx-playground                   feat/pickers                         │
│  ~/dev/notes                            main                     no origin   │
└──────────────────────────────────────────────────────────────────────────────┘
 type to filter  up/down move  enter pick  esc cancel

┌ new session ─────────────────────────────────────────────────────────────────┐
│repo       ~/dev/sbx                                                          │
│clones     https://github.com/you/sbx.git                                     │
│                                                                              │
│task       fix the readme typo                                                │
│name       fix-the-readme                                                     │
│base       main                                                               │
│policy     < feature-work >  clone, agent, push (github + azure devops)       │
│providers    [x] claude-oauth          claude-code-oauth                      │
│             [ ] azure-pat             azure-devops-pat                       │
│                                                                              │
│ staying on the host: 9 uncommitted file(s), 2 unpushed commit(s)             │
└──────────────────────────────────────────────────────────────────────────────┘
 tab field  </> policy  space provider  enter create  esc back
```

The repository on disk is how you *name a remote*, not what gets copied: the
sandbox clones `origin` over the gateway exactly as `sbx new --repo` does, so a
checkout with no origin cannot start a session and is marked as such in the
picker rather than hidden. What has not been pushed is not in the clone, which
is what the last line counts. The current branch becomes the base branch, unless
the remote has never seen it, in which case the remote's default branch is used.

The name follows the task until you edit it, and steps around the names already
in use: a second session in a repository that already has one derives
`inet-server-2` rather than refusing to start until you rename it by hand. With
no task typed the repository's own name is the guess, which is exactly when that
collision happens.

The policy is the same three templates `sbx policies` lists. The providers are
the ones the gateway has: the agent's credential and the repository host's are
ticked when exactly one provider of that type exists -- and when there are
several, the ones the last session for the same host and organisation was given.
Two Azure PATs are two organisations and the type alone cannot say which, but
what you used last time for that org can, and it is evidence rather than a guess.
Failing that, nothing is ticked, since a wrong credential fails three steps
later.

The scan looks in the working directory, its parent, `~/dev`, `~/src`, `~/code`,
`~/projects`, `~/work`, `~/repos`, `~/git` and `$HOME` itself, skipping hidden
and dependency directories and never descending into a repository it has already
found. `SBX_REPO_ROOTS` -- colon-separated, like `PATH` -- replaces that list.
The scan runs on the worker and its result is reused, so the picker opens
instantly the second time and refreshes behind you.

Creating runs on its own thread: the list, the panes and the state column keep
working while a sandbox is provisioned, and the new session appears in the list
as `creating`, then `seeding`, then `ready`, before the gateway has been asked
about it. It needs the sandbox image to exist already -- `sbx image build`
streams docker's output, which a TUI cannot survive -- and `sbx doctor` says so
when it is missing.

### Looking at an agent, and typing at one

The last tab is the agent's screen, as the status poll last captured it:

```
   sessions 2                           1 waiting     agent · diff · policy · events          readme-fix

      1. add-tests                      waiting ●     ❯ fix the typo
         sbx/add-tests                   clean 48s
                                                      ● Read README.md
   ▐ 2. readme-fix                      running ●▌
   ▐    sbx/readme-fix               +12/-3 ? 52s▌    ● Fixed the typo on line 1.

   session readme-fix                                 ──────────────────────────────────────────────────
                                                      ❯
   task      fix the readme typo                      ──────────────────────────────────────────────────
   branch    sbx/readme-fix                             ⏸ manual mode on · ← for agents
   agent at  running  Edit  (screen)

   enter attach to it · j/k scroll  │  1-9 jump · D destroy  │  tab view · q quit
```

It is a view, not an attachment, and it is free: the same capture decides the
state column, so watching an agent costs no round trip of its own. It refreshes
faster while you are looking at it, on the interval the diff pane uses, and it
keeps the colour the agent drew -- the capture carries the escape sequences and
`crate::ansi` turns them back into styled text.

Blank space is squeezed out of it, because the sandbox pane is 200x50 and this
one is whatever is left of your terminal. Claude Code draws its output at the top
of the window and its input box at the *bottom*, so an unsqueezed screen in a
short pane is all output and no prompt -- the half that says what the agent is
waiting for. Runs of blank lines collapse to one; the blanks between messages
survive.

`Enter` (or `a`) hands the whole terminal over to the agent, full width, with
`Ctrl-b d` to come back. That is where typing happens: no key routing to get in
the way, the agent's own scrolling, its own mouse support, and nothing between
you and it. On the way back the agent's window is put back to 200x50, because
tmux keeps a window at its last client's size and the status scraper reads that
window -- attaching from an 80-column terminal would otherwise leave the markers
truncated for the rest of the session.

### Ending a session

`D` destroys the selected session: the sandbox is deleted at the gateway and the
record is dropped, the same thing `sbx rm` does. It always asks, and the question
says what would be lost, because a sandbox holds the only copy of whatever the
agent has not published:

```
 confirm  destroy readme-fix?  +12/-3 ? goes with the sandbox  y/n
```

Only `y` proceeds. An unpolled session says `the sandbox and everything in it
goes` rather than claiming a clean tree, and a session still being created is
refused until it finishes -- the create would otherwise write its record back
after the destroy had dropped it. The row disappears as soon as the gateway
accepts the deletion rather than on the next refresh: a deleted sandbox is listed
as `Deleting` for a while, and waiting would show the session coming back as
`dead` first.

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

The events feed is **kept on disk**, one file per session under
`~/.config/sbx/events/`, because the gateway's log is a rolling window and sbx is
what makes it roll: every exec it takes to read a sandbox writes three lines of
its own, so at these poll intervals a 1500-line window covers about two minutes
and held *one* event worth showing. Each fetch is merged into what the session has
already shown, deduplicated and trimmed to the last few thousand, so the feed is a
record rather than a peephole -- and closing the tool no longer looks like it wiped
the log. Destroying a session takes its history with it.

In the policy pane, `w` widens egress to the package registries and `t`
tightens it back, without restarting the agent -- for the task that turns out
to need a dependency installed. Only the network section: the filesystem and
process sections are fixed when the sandbox is created, and the gateway will
accept a change to them, report it as effective, and never enforce it, so the
pane labels them and declines to offer it.

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

Status: early. See [PLAN.md](PLAN.md) for the increments and
[docs/manual-loop.md](docs/manual-loop.md) for the verified setup.
