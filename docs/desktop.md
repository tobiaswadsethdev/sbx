# The desktop application

A window onto an `sbxd`: the sessions it is running, and what is true about each
one. It talks to the server the CLI talks to, over the same protocol, and it is
built out of the same types.

```
   +---------------------------+
   |  webview (React)          |   the panes, and nothing else
   +-------------+-------------+
                 |  tauri commands
   +-------------+-------------+
   |  sbx-desktop (Rust)       |   sbx-client: one pinned TLS connection
   +-------------+-------------+
                 |  https, one token
   +-------------+-------------+
   |  sbxd                     |
   +---------------------------+
```

**The webview never speaks to `sbxd`.** It cannot: the certificate is pinned by
fingerprint, and `fetch` has no say in which certificate it will accept. Asking
someone to click through a warning is how a self-signed server quietly becomes
an unauthenticated one. So the connection is made on the Rust side, by
`sbx-client` -- the same client `sbx --server` uses -- and the webview calls
commands.

## Running it

Linux needs `webkit2gtk-4.1`, `gtk3` and `libsoup3` and their development
headers; Windows needs WebView2, which Windows 11 has already.

```sh
cd apps/desktop
npm install
npm run tauri dev
```

**Run it that way rather than running the binary.** A development build loads
the frontend from Vite's dev server rather than from the bundle, and `npm run
tauri dev` is what starts that server. Launching `src-tauri/target/debug/
sbx-desktop` on its own gives you a window that says `Operation was cancelled`,
or nothing at all -- which reads exactly like a broken frontend and is not one.
That mistake cost an afternoon; it is written here so it costs nobody else one.

A build that stands alone:

```sh
npm run tauri build
```

Debug builds open the web inspector on start. It is the only way to see a
console message from inside that window, and the alternative is guessing.

## The types are generated

Every message the webview sees is generated from the Rust type that produces
it, into `src/gen/`:

```sh
./scripts/gen-bindings.sh
```

CI fails if the checked-in files disagree with what that produces. This is not
ceremony. `State` is lowercase on the wire and `Verdict` is PascalCase -- an
inconsistency that stays because events are persisted as JSONL per session, so
renaming would make every file already on disk unreadable. A hand-written
`e.verdict === "denied"` compiles, runs, and silently paints every denial as
neutral. Generated, it is a type error.

## What it shows

The session list, and five panes: **terminal** (the agent's screen, live),
**diff** (what has changed, and the review you are writing about it), **facts**
(what the session is), **policy** (the rules the gateway is enforcing) and
**events** (every allow and deny it has made).

Policy and events are the two with no equivalent in an ADE built on git
worktrees, and they are why this is worth building rather than adopting one.

## Reviewing, and telling the agent

Three sections, from `ops::repo_diff`: committed work against the base branch,
uncommitted work, and untracked files. The body arrives marked up rather than
structured -- `### ` for a heading, `!!! ` for a notice, a unified diff
otherwise -- and `sbx_core::pane` calls those markers a contract with whatever
draws it. The pane is the second thing to draw one; the TUI's `diff_line` is the
first, and they strip the same two prefixes.

The comments are the half with no equivalent in a code host. They are not going
to a pull request; they are going to an agent that is **still running**. Click
any line of the diff to write one, and the review sits at the bottom until it is
sent.

**A review is one message, sent once.** Telling the agent about each remark as
it is written would interrupt it six times to say six things that belong
together, and the second interruption would land while it is acting on the
first. So the review accumulates and `SendComments` delivers it whole,
grouped by file and in line order, quoting the line each remark was written
against -- the working copy moves under a review, and a line number that has
gone stale is worth less than the line itself.

**It is kept on the server, per session**, beside the events feed and for the
same reason: a client is a window onto a session, and a review half-written when
the window closes is work. It also makes the review the session's rather than
the window's, so a second client sees it and the agent is told once whichever
one sends it.

Delivery is `tmux load-buffer` then `paste-buffer -p`, not `send-keys`. The
difference is everything for text with newlines in it: `send-keys` types a
message a key at a time, so a review of six comments arrives as six
submissions and the agent starts on the first while the rest is still being
typed at it. A bracketed paste is one block of text however many lines it has,
and the single `Enter` afterwards is the submission. The review is cleared only
once the paste has landed, so a sandbox that was briefly unreachable costs the
delivery rather than the work.

## Starting a session

**new** opens a picker, and picking opens a form -- two stages, because they
answer different questions: which repository is a search, and what kind of
session is a handful of fields with defaults good enough to submit on sight.
It is the same shape as the TUI's, for the same reason.

**The repositories are the server's, not this machine's.** A checkout is only a
way of *naming* a remote -- the sandbox clones `origin` over the gateway either
way -- but which checkouts exist is a fact about the machine that will do the
cloning, and `repo_roots` is configured there. So `Repos` and `Inspect` are
requests like any other, and a window pointed at a server on another continent
lists that server's repositories rather than a set of paths it cannot reach.

Nothing in the form decides anything `sbx new` decides differently, and that is
enforced by where the decisions live rather than by care:

* **The name is derived by the server** when the field is left blank, by the
  same `derive_name` the command line uses. A slug rule reimplemented in
  TypeScript would be a second answer to what a session is called.
* **The toolchains arrive ticked**, from `Inspect` on the repository actually
  picked -- the checkout has already answered that question. All of them are
  listed anyway: a form that hid `dotnet` because there is no `.csproj` yet
  would be one you cannot use to start writing one.
* **The credentials arrive ticked too**, by the same rule the TUI uses --
  `ops::preselect_providers`, which moved into the core when this form needed
  it. A session without the agent's credential comes up to a login prompt and
  one without the repository host's cannot clone a private repository, so both
  are ticked where the type identifies exactly one provider; where it does not,
  the providers the last session for that host was given break the tie. A
  config file naming providers replaces the rule rather than adding to it.
* **Skills and MCP servers are shown, not offered.** They are one decision about
  what your agents can reach, made in the server's config file, and
  `NewSession::into_draft` reads them from there rather than from the request --
  so a client cannot attach a tool, or the endpoint the policy then opens for
  it, by asking.
* **A checkout with no origin is shown and refused**, rather than hidden. There
  is nothing for the sandbox to clone, and a repository missing from the list
  looks like a bug in the scan.

`Create` answers as soon as the request is accepted, not when the agent is
running. Creating takes tens of seconds and the states it passes through --
`creating`, `seeding`, `ready` -- are already on the session and already polled,
so a request that waited would hold a connection open for a minute to say what
the list was about to say anyway. Everything that can be judged from the request
is judged before it returns: an unknown toolchain and a name that is not a name
come back as errors against the request that caused them.

One difference from the TUI worth naming rather than hiding: the picker's filter
is a substring match, where the TUI ranks with the fuzzy score in
`repos::score`. The alternative to a second copy of that scorer in TypeScript is
a request per keystroke, and of the three a plainer match on the same list is
the one that cannot go quietly wrong. If they ever need to agree exactly, the
scorer moves to the server.

## The font metrics WebKit gets wrong

The terminal pane did not draw at all in WebKitGTK, which is the engine Tauri
links against on Linux; once it drew, every row was sheared off two pixels short
of the top. One cause underneath both: WebKit reports a font's vertical metrics
wrongly, and xterm believes them. `src/charSize.ts` corrects both under a single
probe, and does nothing on an engine that measures correctly.

**The cell had no height.** xterm sizes its grid by measuring one character, and
everything else is that measurement: a zero-sized cell is a renderer that skips,
a `FitAddon` that returns `undefined` instead of a column count, and a pane that
stays blank with the agent's screen sitting in the buffer behind it.
`buffer.active.getLine(1)` read `▐███▌  Claude Code v2.1.251` the whole time.

`CharSizeService` measures with a canvas where it can and falls back to
measuring a DOM element where it cannot, and the test it applies is whether
`fontBoundingBoxAscent` and `fontBoundingBoxDescent` **exist**. In WebKitGTK
they exist and are always zero -- for an ordinary canvas as much as an offscreen
one -- so the height comes out zero and the fallback is never reached. Zero is
then read as "the element is `display:none`, keep the value from last time", and
there is no value from last time. Measured in 2.52.6 at `13px monospace`:

```
canvas  measureText('W')  ->  width 7.8,     ascent + descent 0
DOM     offsetWidth / 32  ->  width 7.8125,  height 17
```

So the canvas is asked whether it can measure a height, and where it cannot,
`OffscreenCanvas` is taken away for exactly as long as xterm is choosing a
strategy -- which is inside `Terminal.open` and nowhere else. xterm throws,
catches its own throw, and measures the DOM, which on this engine is right.

**Then the rows were drawn too high.** The DOM fallback measures a span whose
`line-height` is `normal`, so the height it returns *is* the font's natural line
box. xterm takes that number and renders each row with `line-height: <that>px`
-- the same number, and in WebKit not the same thing:

```
line-height: normal   ->  baseline 13px from the top of the row
line-height: 17px     ->  baseline  8px from the top of the row
```

Capitals ink 10px above the baseline and brackets 12px, and rows are
`overflow: hidden`, so at a baseline of 8 the top of every line is shaved:
`README` reads as `KEADME`, `HEAD` as `HEAU`. Raising xterm's `lineHeight`
option is not a fix -- it moves the baseline by only half of what it adds, and
could not lift brackets clear at any row height anyone would want. Restoring
`line-height: normal` on the rows' spans puts the baseline back at 13 and costs
nothing, and it is not a nudge until it looks right: the cell height was
*measured* as the natural line box, so rendering at the natural line box is the
only setting consistent with it. That equivalence would break if xterm's
`lineHeight` option moved off 1, which is why nothing does that.

Both are worth reporting upstream. The first belongs in that constructor, which
should measure once and reject a strategy that returns nothing rather than trust
a property that exists but does not work.

Two things this cost that are worth not re-deriving. The renderer *was* running
even when nothing appeared -- `.xterm-rows` had its row elements and their text
-- so a DOM inspection that stops at "is the text there" says everything is
fine. Every row simply had no height. And ruling out WebKit's sandbox,
`requestAnimationFrame`, font availability and a missing stylesheet ruled out
nothing, because the failure was never in any of them.

Verified in the running application, not only in a harness: against a live
`sbxd` and a real sandbox, the pane draws the sandbox's shell, renders colour
from `git log`, paints bytes as they arrive, sends what is typed into it, and
draws `README HEAD 7fd1a60 EIT013 pqy|{[()]}` with every glyph whole. `tput
cols` inside the sandbox reads **113**, matching `tmux list-clients` -- so the
grid is sized from a measured cell rather than the 80x24 a zero cell used to
leave behind.

Whether WebView2 on Windows was ever affected is still untried. The probe makes
it moot: if its metrics work, nothing here changes.

## Wayland, X11, and WSLg

The window is a Wayland client wherever there is a Wayland compositor, WSLg
included, and nothing here overrides that. `GDK_BACKEND=x11` puts it through
XWayland, which is worth knowing only because X11 screenshot tooling can see an
XWayland surface and cannot see a Wayland one.

If you do capture it that way, capture the *window*, not a region of the screen:

```sh
WID=$(xdotool search --name '^sbx$' | head -1)
ffmpeg -f x11grab -window_id $WID -i $DISPLAY -frames:v 1 -y shot.png
```

A region grab of a redirected window comes back solid black, which looks like a
renderer that has failed and is a capture that has.

---

[← Documentation](README.md) · [README](../README.md)
