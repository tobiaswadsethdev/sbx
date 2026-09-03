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

## Connecting it to a server

**It needs a server to talk to**, and saying which is the first thing a new
window asks. On the machine with the sandboxes:

```sh
sbxd serve                              # or under systemd; see server.md
sbxd pair desktop --host 127.0.0.1      # ... or the address the window will dial
```

`pair` prints one line -- an address, a token, and the fingerprint of the
certificate the server will present. Paste it into the window: **paste a
pairing string** on the empty screen, or **servers** in the header once
something is paired. The name is optional and defaults to the host.

```
   +-------------------------------------------------------+
   |  Connect to a server                           close   |
   |                                                        |
   |  pairing   sbx://box.lan:17671/8f3c…#d8fa…             |
   |  name      work                                        |
   |                                                        |
   |  paired    wsl        127.0.0.1:17671        forget    |
   |                                             [connect]  |
   +-------------------------------------------------------+
```

`sbx connect 'sbx://…'` in a terminal does the same thing, and a server paired
either way appears in both -- they are one saved list (`~/.local/state/sbx/remotes.json`,
or `%LOCALAPPDATA%\sbx\remotes.json` on Windows) and one implementation:
`sbx_client::pair`, called by the command and by the dialog. Two implementations of "is this a
server I can talk to" would be one implementation and one place a mistake is
silent.

**The dialog is there because the machine holding the window may have no `sbx`
on it.** On Windows there is none to install: the CLI drives Docker, tmux and a
gateway, which are on the Linux side. Requiring a terminal to pair would have
made the Windows client depend on a program that cannot run there.

What it does with the string is what `connect` does. It parses it, dials the
address, accepts the certificate only if it matches the fingerprint the string
carries, checks that what answered is an `sbxd` speaking this protocol version
-- and saves nothing until all of that has happened. A pairing string that names
nothing fails there, in front of you, rather than on every request afterwards.
What comes back on success is the server's own version, which is the one thing
a paste cannot fake.

The string is a credential, so it is never echoed back into an error message:
the errors say what is wrong with the *shape* of a pairing string, never what
was pasted.

**`--host` is the one to get right.** Without it the string carries the server's
own hostname, which is often not what the client should dial and on a
Debian-family box resolves to `127.0.1.1` while `sbxd` is bound to `127.0.0.1`
-- `Connection refused` from a server that is running perfectly well.
[server.md](server.md) has the two-machine case in full, and the WSL case, where
the address depends on how WSL is networked.

**forget** drops the token this machine holds and nothing on the server. The
server stops accepting one when `sbxd revoke` says so, which is the half that
matters if a pairing string has been somewhere it should not.

## Running it

Installing it is [install.md](install.md#the-desktop-application): a Windows
installer from the release page, or built from the tree on Linux, where
`webkit2gtk-4.1`, `gtk3` and `libsoup3` and their development headers are the
prerequisite.

From a checkout:

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

A workspace, not a list. **Projects** on the left containing **worktrees**, what
you have open in the middle as tabs, and a **dock** on the right carrying what
is true about the selected worktree.

```
   +-----------+--------------------------+-----------+
   | projects  |  agent | diff | ...       |  events   |
   |  worktree |                          |  policy   |
   |  worktree |   whatever is open       |  facts    |
   | projects  |                          |           |
   +-----------+--------------------------+-----------+
```

It was a flat session list, and that was right while a session was the unit of
work. It stopped being right at four repositories: a list sorted by name says
nothing about which four they are.

**A project is a decision, not a discovery.** `repos::discover_in` finds every
checkout on the machine, which is tens of them; a project is the handful someone
has said they are working on, made by picking one. So it is stored rather than
derived from the sessions that exist -- a project with no worktrees yet is the
normal state of one you just made, and grouping sessions by clone URL could
never represent it.

A worktree records the project it was started in rather than being matched back
to one by URL, because two projects may share a URL: two checkouts of one
repository is a normal thing to have, and the worktree would otherwise belong to
both. Anything with no project -- everything `sbx new` creates, since the
terminal has none -- is grouped by clone URL at the bottom of the tree rather
than hidden. Forgetting a project leaves its worktrees alive and moves them
there; a sandbox is a real thing with an agent in it, and removing one is
`sbx rm`'s job, said out loud.

**A worktree with no sandbox around it says so on its row.** The `worktree`
badge beside the name means the session runs on the server with the server's own
rights: no policy, no allow/deny feed, and a publish that uses the server's git
credentials. The policy and events panes say the same thing where their contents
would be, in the server's own words rather than a wording kept here, and the
facts pane shows `isolation: none` with the directory instead of a sandbox name.
[worktrees.md](worktrees.md) is what it buys and what it costs.

**Tabs are per worktree.** A tab is a thing you have open *in* a working copy,
so switching worktree switches the set and coming back finds it as you left it.
Every tab stays mounted and is hidden rather than unmounted: a terminal that
unmounts closes its channel and detaches, so switching to the diff and back
would lose the screen and re-attach.

**`+` opens another shell** in the same sandbox, under the same policy. It is
not a way around the isolation; it is a second prompt inside it, which is the
point -- you can run the tests while the agent is still working. Each shell is
its own tmux session in the sandbox, so they do not contend: `attach -d` evicts
a client left behind by a crash, and two tabs on one tmux session would evict
each other instead.

What shells exist is asked of the **sandbox**, not remembered here. tmux already
knows, and its answer outlives this window closing, the server restarting and a
second window opening -- a list kept in a client would show a shell that had
been closed from elsewhere and hide one opened from elsewhere. The server also
names them, because two windows adding a shell at once would otherwise both
pick `shell-2` and the second would silently attach to the first's.

Closing a shell kills what is running in it, so only a shell has the button. The
agent's terminal is not one, and `kill_shell` refuses a target that is not
prefixed `shell-` rather than trusting the request -- closing the agent's tab
must not stop the agent.

**The dock is not a tab bar, and that is deliberate.** Files, git, facts, policy
and events sit in a sidebar beside the editor. Files and git are places you work
*from* -- look at what changed, open it, come back -- and a diff you are reading
should not have to give up its place so you can see what else changed. Facts,
policy and events are what is true about the worktree, and keeping them one
click away rather than behind a tab is the point: the isolation being *visible*
is the reason this is worth building rather than adopting an ADE built on git
worktrees, and a denial you have to go looking for is one you will not find. It
costs width the editor would otherwise have; that is the trade.

## Starting work

**new project** opens the picker; picking a checkout makes the project. Then
**+** beside a project starts a worktree in it, which is the form without the
repository question -- the project is the standing answer to that one, and what
is left is the part that differs between one worktree and the next.

The repositories in the picker are the **server's**. A checkout only ever names
a remote -- the sandbox clones `origin` over the gateway either way -- but which
checkouts exist is a fact about the machine that will do the cloning, and
`repo_roots` is configured there. So `Repos` and `Inspect` are requests like any
other, and a window pointed at a server on another continent lists that server's
repositories rather than a set of paths it cannot reach.

**Where it runs** is the first question, because it decides which of the others
mean anything. A sandbox is the default and the point; a worktree is seconds
instead of minutes and gives up every guarantee, so the form spells that out
beside the choice and again as a notice once it is picked. Picking it hides the
policy, toolchain and credential fields rather than disabling them: each is an
instruction to a gateway that will not be involved, and a greyed-out policy
chooser suggests a choice that has been taken away when the truth is there is
nothing to apply one to. The command line refuses those flags outright for the
same reason.

Nothing in the form decides anything `sbx new` decides differently, and that is
enforced by where the decisions live rather than by care:

* **The name is derived by the server** when the field is left blank, by the
  same `derive_name` the command line uses. A slug rule reimplemented in
  TypeScript would be a second answer to what a session is called.
* **The toolchains arrive ticked**, from `Inspect` on the project's checkout --
  it has already answered that question. All of them are listed anyway: a form
  that hid `dotnet` because there is no `.csproj` yet would be one you cannot
  use to start writing one.
* **The credentials arrive ticked too**, by the same rule the TUI uses --
  `ops::preselect_providers`, which moved into the core when this form needed
  it. A session without the agent's credential comes up to a login prompt and
  one without the repository host's cannot clone a private repository, so both
  are ticked where the type identifies exactly one provider; where it does not,
  the providers the last session for that host was given break the tie. A
  config file naming providers replaces the rule rather than adding to it.
* **The base branch is the checkout's own.** `Inspect` resolves it server-side
  when the request does not name one, which is what `None` on that request has
  always meant. Unresolved, `base_on_remote` reports every branch as missing
  from the remote and the form silently falls back to the remote's default --
  which is what it did for about an hour.
* **Skills and MCP servers are shown, not offered.** They are one decision about
  what your agents can reach, made in the server's config file, and
  `NewSession::into_draft` reads them from there rather than from the request --
  so a client cannot attach a tool, or the endpoint the policy then opens for
  it, by asking. A worktree session is given neither and the form says why: its
  agent is the server's own, reading that user's `~/.claude` already.

`Create` answers as soon as the request is accepted, not when the agent is
running. Creating takes tens of seconds and the states it passes through --
`creating`, `seeding`, `ready` -- are already on the worktree and already
polled, so a request that waited would hold a connection open for a minute to
say what the tree was about to say anyway. Everything that can be judged from
the request is judged before it returns: an unknown toolchain and a name that is
not a name come back as errors against the request that caused them.

One difference from the TUI worth naming rather than hiding: the picker's filter
is a substring match, where the TUI ranks with the fuzzy score in
`repos::score`. The alternative to a second copy of that scorer in TypeScript is
a request per keystroke, and of the three a plainer match on the same list is
the one that cannot go quietly wrong. If they ever need to agree exactly, the
scorer moves to the server.

## The working copy

The file tree is one of the dock's views, and opening a file opens a tab. One
directory per request, expanded as it is opened: a repository is tens of
thousands of files, every listing is an exec into the sandbox, and a tree only
ever shows the branches someone has opened. Collapsing a directory forgets it,
so reopening re-reads -- the agent is still editing, and a tree cached from an
hour ago would be a tree of what used to be there.

**Read-only, because the agent owns the working copy.** Two writers with no
shared lock is how a file ends up with half of each. What you want here is to
see what it did and say something about it, which is what the review is for.

Paths are checked on the server rather than trusted, by component rather than by
looking for `..` in the string -- `a/../b` is fine and `..config` is a real
filename, and a component that *is* `..` is the one case that escapes. Contents
come back base64: an exec's stdout is already lossy UTF-8, so a source file with
a stray byte in it would otherwise come back altered. A NUL in the first block
means binary, which is what git decides on too and is right more often than any
extension list.

### Monaco, and the worker that is not optional

The viewer is Monaco, and it renders correctly in WebKitGTK -- measured before
anything was built on it, in the same harness that found the terminal's font
metrics, because that engine had already cost two bugs. Character width comes
back as 8.4 rather than the zero xterm's canvas path returns, and the editor
paints with highlighting, line numbers and no clipping.

**But Monaco computes its diff in a web worker, and without one it fails
quietly.** The editor still renders; the diff editor shows two panes with no red
or green in them, which reads as an empty diff rather than a missing worker. The
probe caught it because it counted decorations instead of trusting the
screenshot: three with the worker configured, zero without. Vite needs the
worker named explicitly, and the specifier is `monaco-editor/editor/...` --
Monaco 0.56's export map is `"./*": "./esm/vs/*.js"`, so the path everyone
writes from memory does not resolve.

What is imported matters as much. `import * as monaco from "monaco-editor"`
brings the language *services* -- IntelliSense for TypeScript, CSS, HTML and
JSON -- which is four more workers and takes the bundle to 15MB, to power
completions in a viewer that cannot be typed into. The editor API plus
`basic-languages`, which is the tokenising on its own, is 4MB and keeps the one
worker that computes diffs.

## Git

The dock's **git** view is the working copy as git describes it: the branch, how
far it has diverged, what is staged and what is not. Clicking a changed file
opens its diff as a tab -- Monaco's side-by-side editor, `HEAD` against the
index for something staged and the index against the working copy for something
not, which is the distinction staging exists for.

Staging, unstaging, discarding, commit, push, pull and fetch. `push` uses `-u`
every time, not only the first: it is a no-op once set, and without it a branch
that has never been pushed has no upstream to report ahead and behind against
afterwards -- which is why the button says **publish** until there is one.
`pull` is `--ff-only`, because a merge commit made behind the agent's back, in a
working copy it is still editing, is not a thing to do on a button press.

**The agent is editing while this is on screen**, and that is the fact the whole
view is shaped around. A status is a snapshot already slightly out of date.
Staging a file records the version of it that exists at that moment, which may
not be the version that gets committed. Discarding races whatever the agent is
doing to that file, so it asks first and says so. None of this is fixable from
here -- git's index is the only lock there is and the agent does not take it --
so what the view does instead is never pretend otherwise: every action re-reads
the status from the server rather than adjusting the list it already had, and
every one reports git's own words rather than a sentence written here about
them.

## Reviewing, and telling the agent

The comments are the half with no equivalent in a code host. They are not going
to a pull request; they are going to an agent that is **still running**. Click
any line of a file's diff to write one; it is marked in the margin, and the
review waits until it is sent.

They live in the diff editor now rather than in a unified text pane, and nothing
about the review had to change to move them: `sbx_core::comments` has always
stored `{file, line, excerpt}`, which is already per file. That is the reward
for having stored the excerpt rather than a line identity -- the anchor did not
depend on which rendering of the diff it was written against.

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
`README` reads as `KEADME`, `HEAD` as `HEAU`.

**It is not a terminal problem**, which took a second sighting to notice. The
same two pixels come off any element in the window that clips -- a filename with
`overflow: hidden` and `text-overflow: ellipsis`, which is most of a sidebar --
because the body sets `line-height: 1.5` and that is an explicit one too. At
19.5px the baseline lands at 9 and capitals ink 10, so `NOTES.md` renders as
`NOIES.md`: legible enough to read as a font choice rather than a bug, which is
why it survived a whole increment. `charSize.ts` marks the document as well as
the terminal, and one rule on `body` fixes every clipped element at once. Raising xterm's `lineHeight`
option is not a fix -- it moves the baseline by only half of what it adds, and
could not lift brackets clear at any row height anyone would want. Restoring
`line-height: normal` on the rows' spans puts the baseline back at 13 and costs
nothing, and it is not a nudge until it looks right: the cell height was
*measured* as the natural line box, so rendering at the natural line box is the
only setting consistent with it. That equivalence would break if xterm's
`lineHeight` option moved off 1, which is why nothing does that.

**And a third, which is the same zero metrics costing the opposite thing.** A
form control takes its height from its line-height -- rows times that, for a
textarea -- and `.dialog input { font: inherit }` hands it the `normal` the fix
above put on the document, which this engine resolves from metrics it reports as
zero. Every input and textarea in the window collapsed to a sliver with its text
clipped through the middle: a three-row textarea 14 pixels high, a placeholder
sheared in half, and nothing about it that looks like a line-height. It survived
because the fields are usually typed into rather than read, and because a
`<select>` beside them renders correctly -- a native control brings its own
metrics. Found while adding the connect dialog, where a pairing string is the
one thing you *do* read back. These need the explicit line-height the rest of
the document must not have, and the padding they already carry absorbs the two
pixels it puts the baseline out by, so the rule stops at controls.

All three are worth reporting upstream. The first belongs in that constructor,
which should measure once and reject a strategy that returns nothing rather than
trust a property that exists but does not work.

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
