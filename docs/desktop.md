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
