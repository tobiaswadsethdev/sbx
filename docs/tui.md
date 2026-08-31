# The TUI

`sbx` with no arguments opens the TUI: every session in one list, and
whatever one of them is doing beside it.

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

## Starting a session

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
│tools        [ ] dotnet     the .NET SDK, and nuget                           │
│             [ ] node       node and npm (already in the base image), and t..  │
│             [x] rust       rustc, cargo, fmt and clippy, and crates.io       │
│providers    [x] claude-oauth          claude-code-oauth                      │
│             [ ] azure-pat             azure-devops-pat                       │
│                                                                              │
│ staying on the host: 9 uncommitted file(s), 2 unpushed commit(s)             │
└──────────────────────────────────────────────────────────────────────────────┘
 tab field  </> policy  space toggle  enter create  esc back
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

`tools` is the toolchains the sandbox image should carry, and it usually arrives
answered: a checkout with a `Cargo.toml` in it comes up with `rust` ticked, one
with a `.csproj` a level down with `dotnet`. Each set of toolchains is its own
image variant, so a session asking for one that has not been built yet is
refused with the `sbx image build --toolchain ...` that builds it -- that build
streams docker's output, which a full-screen interface cannot host.
[toolchains.md](toolchains.md) covers what each one installs and what it may
reach.

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

## Looking at an agent, and typing at one

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

## Ending a session

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

## Names and branches

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


---

[← Documentation](README.md) · [README](../README.md)
