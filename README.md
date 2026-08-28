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

## Configuration

Everything above is a flag, and `sbx config --init` writes a file that stops them
being typed. `~/.config/sbx/config.toml`, beside the session cache, all keys
optional:

```toml
gateway    = "openshell"                              # unset: the active one
repo       = "https://github.com/octocat/Hello-World" # `sbx new` with no --repo
base       = "develop"                                # unset: the remote's default
policy     = "feature-work"                           # a template, or a path to a YAML file
providers  = ["claude-oauth", "azure-pat"]            # credentials for a new session
repo_roots = ["~/dev", "~/work"]                      # where the picker looks
refresh    = "1s"                                     # how often the TUI reads the sandboxes

skills     = ["ship-pr"]                               # copied into every session

[[mcp]]                                               # one table per MCP server
name = "jira"                                         # see "MCP servers" below
url  = "http://mcp-atlassian:9000/mcp"
```

Everything in it is a *default*: a flag on the command line wins, and so does an
explicit choice in the create form. `sbx config` prints what is in force with
`*` for the file's answers and `-` for the built-in ones.

**A file that cannot be read stops the command**, rather than being quietly
replaced by the defaults -- a key that does nothing is indistinguishable from a
key that is not working, so a misspelled one is named back at you:

```
sbx: ~/.config/sbx/config.toml: TOML parse error at line 1, column 1
  |
1 | polciy = "feature-work"
  | ^^^^^^
unknown field `polciy`, expected one of `gateway`, `repo`, `base`, `policy`, ...
```

The one exception is `sbx doctor`, which is the command you reach for when
something is wrong: it reports the error as a failed check and carries on with
the defaults. It also checks the `providers` you named still exist at the
gateway, since a stale name is the quietest failure here -- the form does not
tick it, the sandbox comes up without the credential, and the clone fails for
what looks like an authentication problem several steps later.

`refresh` is one number rather than six because the intervals underneath it are
measured and related to each other; it scales all of them, so `"4s"` polls a
quarter as often (41 execs in a 30 second window became 13) and `"500ms"` twice
as often. 250ms to 60s -- below that the TUI's 100ms input tick becomes the
limit and the extra `git status` inside every sandbox buys nothing.

Where a default meets something sbx already works out for itself, the more
specific answer wins:

* `providers` **replaces** the create form's guesswork, because an explicit list
  beats a heuristic and merging the two would attach a credential nobody asked
  for.
* `base` only fills a **detached HEAD**: the branch a checkout is sitting on is
  evidence about that repository, and a config entry is a guess about all of them.
* `repo` moves the picker's **cursor**, not its filter, so every other repository
  is still one keystroke away -- and typing drops the preference for good.
* `repo_roots` **replaces** the conventional places rather than adding to them,
  and `SBX_REPO_ROOTS` still wins over it.

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
| `attribution` | `commit` and `pr` both empty, so nothing is stamped -- an empty string is what silences it, where an absent key means the default trailer |
| `copyOnSelect` | off. Not a `settings.json` key -- it lives in the global `.claude.json` the image also writes, and defaults to on; selecting text to read it should not take the clipboard of a terminal you are borrowing |
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
`j`/`k` scroll rather than walk the list -- except in the events feed, where they
move a cursor over the events and the pane scrolls to follow it. The footer always says what the keys are
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

### Names and branches

A session name is derived from the task, and the words a task opens with are
almost never what it is about: "I want to add the MaxGaming Scala customer id"
used to become `i-want-to-add`, which spends the whole budget on the wrapper and
names nothing. Filler -- pronouns, articles, auxiliaries, `want`, `please` --
is dropped, and verbs are kept, because `add the flag` and `remove the flag` are
two different sessions. A task made of nothing else still gets a name: the
filtered pass is tried first and the text as written is the fallback.

Names run to 40 characters, longer than a sandbox name can hold. The gateway
caps those at 19, so `sbx-` leaves 15 -- and rather than cap the name there,
the *sandbox* name is derived from it: short names are `sbx-<name>` exactly, and
a longer one keeps its first ten characters and ends in four hex digits of the
whole name. That keeps it a pure function of the session name, which is what
lets `sbx rm` and adoption name a sandbox with no record to read, while two
names sharing fifteen characters still get two sandboxes. The full name travels
in the `sbx.session` label, which has 63 characters to spend.

```
I want to add the MaxGaming Scala customer id
  session   add-maxgaming-scala-customer-id
  branch    sbx/add-maxgaming-scala-customer-id
  sandbox   sbx-add-maxgam-0c45
```

The branch stays `sbx/<name>`, and the task field in the create form wraps over
four rows and scrolls with the cursor, since a prompt is a sentence and a single
row shows you the end of one with the cursor past the edge of the modal.

## Skills

Your skills do not follow you into a sandbox: it has its own `HOME`, so a fresh
one has none. Name them in the config file and every new session gets them:

```toml
skills = ["ship-pr", "~/dev/notes/.claude/skills/changelog"]
```

A bare name is one of your own, under `~/.claude/skills` (or
`$CLAUDE_CONFIG_DIR/skills`). Anything with a `/` in it is a path, so a skill
that lives in a repository can be pointed at where it actually is.

**It is a copy, and it cannot be anything else.** A symlink does not cross into
a sandbox and a bind mount would hand it the rest of `$HOME` -- the isolation is
the whole product. What the config file holds is the *pointer*, which buys the
part of a symlink that is actually wanted: edit the original, and the next
session gets the edit. A running session keeps what it was created with, and its
record and facts pane say what that was.

The whole directory travels, not just the manifest: `SKILL.md` beside its
scripts, references and templates, packed with `tar` on the host and unpacked
into `/sandbox/.claude/skills` as a seeder step before the agent starts. Symlinks
inside are followed, so a skill that is itself a link arrives as its contents.
A skill above 256KiB packed is refused rather than silently making the create
fail on an over-long command line -- at that size something has a virtualenv in
it by accident.

A skill that is missing at create time costs the skill, not the session: it is a
warning, and `sbx doctor` says so beforehand, since a session that quietly comes
up without one looks like the agent forgetting how to do something it used to
know.

```
[ warn ] skills       ship-pr: /home/you/.claude/skills/ship-pr has no SKILL.md, so the agent would not load it
```

## MCP servers

The agents can be given MCP servers, and the servers run **on the host, in their
own containers, holding their own credentials**. Nothing about Jira or Azure
DevOps ever lands on a sandbox filesystem; the sandbox is granted one endpoint
per server, and the grant is per-binary like every other rule here:

```
claude → POST http://mcp-azure-devops:9001/mcp   ALLOWED  [policy:allow_mcp_azure_devops_9001]
curl   → POST http://mcp-azure-devops:9001/mcp   DENIED   [binary '/usr/bin/curl' not allowed]
```

Same host, same port, different binary -- measured from inside a session, not
described. Claude Code 2.x is a native binary, so `/usr/local/bin/claude` is a
rule only the agent satisfies. That is sharper than the registry rules in
`net-open.yaml`, where npm's kernel-resolved exe is `/usr/bin/node` and the rule
cannot tell an agent from anything else JavaScript in the sandbox.

Name them in the config file, one table each:

```toml
[[mcp]]
name = "jira"
url  = "http://mcp-atlassian:9000/mcp"

[[mcp]]
name = "azure-devops"
url  = "http://mcp-azure-devops:9001/mcp"
transport = "http"                        # or "sse"; http is the default
```

The url is what the **sandbox** sees, which is not what your browser sees.
`localhost` in there is the sandbox itself and is refused when the file is read,
because it is correct on the host, wrong in the sandbox, and invisible until an
agent is running. Two addresses work instead:

* **the container's name**, when it has joined the gateway's own Docker network
  with `--network openshell-docker`. Docker's embedded DNS resolves it even
  though the sandbox has no DNS of its own, because the proxy does the
  resolving -- and nothing is published on the host at all. This is the shape to
  prefer.
* **`host.openshell.internal`**, which every sandbox already has in `/etc/hosts`
  pointing at the bridge gateway, for a server that is not in a container or is
  in one that cannot join another network. Publish to the bridge address rather
  than to `127.0.0.1`, or the sandbox cannot reach it.

Jira and Confluence, with the credentials staying in the container:

```sh
docker run -d --name mcp-atlassian --network openshell-docker \
  -e JIRA_URL=https://your-org.atlassian.net \
  -e JIRA_USERNAME=you@example.com -e JIRA_API_TOKEN="$JIRA_API_TOKEN" \
  ghcr.io/sooperset/mcp-atlassian:latest --transport streamable-http --port 9000
```

Azure DevOps needs one extra part: `@azure-devops/mcp` speaks stdio only, so it
runs behind an HTTP shim. Its `pat` mode reads `PERSONAL_ACCESS_TOKEN`, and wants
the base64 of `:<pat>` -- it decodes the value and drops everything up to the
first colon, which is Azure DevOps' usual empty-username Basic auth:

```sh
docker run -d --name mcp-azure-devops --network openshell-docker \
  -e PERSONAL_ACCESS_TOKEN="$(printf ':%s' "$AZURE_DEVOPS_PAT" | base64 -w0)" \
  node:22-alpine npx -y supergateway \
    --stdio "npx -y @azure-devops/mcp <org> -a pat" \
    --outputTransport streamableHttp --port 9001 --stateful
```

Both serve `/mcp`. The Azure DevOps one was run against a real session while
writing this -- the agent reported `azure-devops: ... ✔ Connected`, with Azure
DevOps MCP 2.9.0 answering behind the shim, and the denial above is `curl` in
that same sandbox. The Atlassian one was started and answered on `/mcp` with
placeholder credentials; its own flags are documented by that image.

Registration happens **inside the sandbox, before the agent starts** -- the
seeder runs `claude mcp add --scope user` as its own `mcp` step, because the
agent reads its servers at startup and registering them afterwards would leave
the first session of every sandbox without tools. The endpoints are opened in
one `policy update` at creation, so the rules are loaded before anything can use
them. A session records the servers it was created with, and the facts pane
lists them by name; changing the file changes the next session, not a running
one.

`sbx doctor` checks each of them, because a container that is not running -- or
one running but not attached to the gateway's network -- produces a session whose
agent reports its tools as **needing authentication**, which sends you looking in
entirely the wrong direction:

```
[ warn ] mcp          jira: there is no container named `mcp-atlassian`, so no sandbox can resolve that url
         fix: start it, or attach it with `docker network connect openshell-docker <container>`; or fix its url in the config file
```

**What this costs.** An MCP server is a hole in the sandbox, and worth being
plain about: the agent gains everything the server can do, using the host's
credentials, and the gateway can only see it as `POST /mcp`. Every MCP call is
the same request shape, so the method/path rules that make the git endpoints
sharp buy nothing here -- a server that can transition Jira issues means a
sandboxed agent can transition Jira issues. That is a fine trade for Jira and
Azure DevOps, whose blast radius is a work item. It is a terrible one for a
filesystem or Docker MCP server on the host, which would be a straight sandbox
escape, and sbx cannot tell the difference for you.

The transport is not a problem the way it might look: streaming responses are
not buffered by the inspecting proxy. An SSE stream emitting an event a second
arrived event by event, a second apart, inside the sandbox.

## Policy

The isolation is the point, so it is visible rather than buried in a YAML file.
The **policy** pane shows the rules the gateway is actually enforcing, per
binary, and the **events** pane is the allow/deny feed behind them:

```
┌ events (UTC) - add-tests ───────────────────────────────────────────────────┐
│  11:15:02  allow  GET github.com:443/octocat/Hello-World.git/info/refs [git]│
│▌ 11:15:02  DENY   /usr/bin/curl(93) -> pastebin.com:443                     │
│▌           endpoint pastebin.com:443 is not allowed by any policy           │
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

### Acting on a denial

`w` and `t` are one preset. The events feed is where the *specific* answer lives:
`j`/`k` move a cursor over the events, and `e` on the one you are looking at asks
what to do about the endpoint it names.

```
 endpoint  pastebin.com:443 for /usr/bin/curl  -- denied now
           a allow here · b block here  │  A allow always · B block always  │  esc cancel
```

Lowercase changes this session, through the same live `policy update` that `w`
uses; uppercase does that *and* records the endpoint in a global list applied to
every `sbx new` from then on. Nothing else on the keyboard responds while the
question is up -- `a` is attach everywhere else in the TUI, and answering a
question about egress must not also hand over the terminal. Any other key
cancels.

An allow binds the endpoint to **the binary the event named**, not to the
sandbox: allowing `github.com:443` off a denied `curl` grants it to curl and
leaves git's own rule alone. That is also why an event decided by an L7 rule --
`GET httpbin.org:443/ip`, which names a method and a path and no binary -- can be
blocked but not allowed: an endpoint rule with no binaries grants nothing, and
issuing one would report a change that did nothing.

**A block is a removal, not a veto.** OpenShell denies by default and has no
deny-that-outranks-an-allow at L4, so blocking `pastebin.com` is a no-op -- it was
never reachable -- and blocking `platform.claude.com` is real, because
`feature-work.yaml` grants it. The pane says which, per entry:

```
── global lists - applied to every new session
  allow       pastebin.com:443  NOT in this policy
              /usr/bin/curl
  block       platform.claude.com:443  STILL in this policy
  block       nowhere.example.com:443  gone from this policy
```

The third column is the point: a list entry describes what a *new* session gets,
and the session in front of you may predate it or have moved since. The lists
live in `~/.config/sbx/endpoints.json`, are written under a lock like the session
cache, and are applied to a fresh sandbox in one `policy update` before the clone
starts -- so nothing has run in it yet. A block that fails to apply **fails the
create**; an allow that fails is a warning. The two are not symmetric: a missing
allow announces itself the moment the agent tries, and a missing block never
mentions itself again.

There is no key for taking an entry off a list -- `A` and `B` move an endpoint
between them, and removing it outright means editing the file, which is plain
JSON and hand-editable.

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
