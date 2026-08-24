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
sbx diff <name>                               # what the agent has changed so far
sbx rm <name>                                 # delete session and sandbox
sbx                                           # the TUI
```

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
 j/k scroll  pgup/pgdn page  h pane  tab preview/diff  enter attach  q quit
```

The state column is what the *agent* is doing, not just whether the sandbox is
up. A session blocked on a permission prompt shows `waiting` as a filled badge
and is counted in the title, so you can see it without scrolling to it -- that
notification is the reason to run several sessions at once. It comes from
scraping the agent's screen as well as from hooks baked into the image, because
Claude Code fires no hook for a permission prompt or an interrupt; the preview
pane says which source decided.

`Tab` cycles the right pane between the preview and the diff, remembered per
session. `h`/`l` move focus between the panes, and the movement keys follow it:
`j`/`k` walk the session list on the left and scroll on the right. The `+12/-3`
column counts lines changed against the branch the session started from, and
`?` marks untracked files. Both panes refetch on a timer, so a diff you are
reading keeps up with the agent editing underneath it.

The local cache is disposable: each session's record lives inside its own
sandbox, so deleting `~/.config/sbx/sessions.json` and running `sbx ls`
re-adopts everything still running.

Status: early. See [PLAN.md](PLAN.md) for the increments and
[docs/manual-loop.md](docs/manual-loop.md) for the verified setup.
