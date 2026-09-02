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

The session list, and three panes: **facts** (what the session is), **policy**
(the rules the gateway is enforcing) and **events** (every allow and deny it has
made). Read-only so far. The terminal, the diff with comments, and creating a
session are increments of their own.

Policy and events are the two with no equivalent in an ADE built on git
worktrees, and they are why this is worth building rather than adopting one.

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
