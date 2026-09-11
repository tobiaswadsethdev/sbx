// The agent's screen, live.
//
// xterm.js over the terminal channel: bytes out of the sandbox's tmux into the
// emulator, keystrokes back. Nothing here interprets the stream -- the escape
// sequences are the agent's, and the one thing this must not do is try to
// understand them.
//
// `xterm.open` goes through `withUsableFontMetrics` because WebKitGTK reports a
// font's vertical metrics wrongly and xterm believes them. See charSize.ts:
// without it the character cell is zero -- a pane that stays empty however much
// arrives in the buffer behind it -- and every row it does draw is sheared off
// two pixels short of the top.

import { useEffect, useRef } from "react";
import { Terminal as Xterm } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";

import { withUsableFontMetrics } from "../charSize";
import { palette } from "../palette";
import { close, decodeBytes, encodeBytes, nextChannelId, open, terminal } from "../stream";

/// Which custom property each of xterm's colours comes from, and what to fall
/// back to. See `palette.ts` for why this is resolved rather than written down.
const THEME = {
  background: ["--sunken", "#0a0a0a"],
  foreground: ["--text", "#fafafa"],
  // No accent to borrow: the window's hues mean "working", "needs you", "good"
  // and "bad", and a cursor is none of those. Near-white on near-black, which
  // is how everything else in the window takes emphasis.
  cursor: ["--text", "#fafafa"],
  // Nothing for the scrollbar, because this pane does not draw one -- see
  // style.css. Three colours for a slider that is `display: none` would be
  // three more things to keep in step with a palette for no pixels at all.
} satisfies Record<string, [string, string]>;

/// How many characters fit in the pane, across and down.
///
/// This is `FitAddon`'s job and it is not used for it, because of one line in
/// it: the width it fits into is the pane's *minus a scrollbar's*, always, on
/// the assumption that the scrollbar sits beside the text. This one does not --
/// xterm 6 draws it as an element positioned over the right of the screen, and
/// it fades out when the pointer is elsewhere. So the fourteen pixels were being
/// held open for something that was never going to occupy them, and the agent's
/// screen was fourteen pixels narrower than the pane for it. Which is invisible
/// until the agent draws a rule across its own full width -- and then it is a
/// line that stops short of an edge it is plainly meant to reach.
///
/// The cell is measured off what xterm has already drawn rather than asked for:
/// the screen element is exactly `cols` by `rows` cells, so dividing gives the
/// cell without reaching into `_core` for the render service, which is the
/// private API `FitAddon` carries a `TODO` about. Before the first render the
/// grid is xterm's default 80x24 and the screen has a size, so this holds from
/// the first call.
///
/// Up to one cell is still left over at the right and the bottom: a grid cannot
/// draw a fraction of a column, and rounding up would clip one instead. That
/// residue is the terminal's, not the pane's.
function gridFor(element: HTMLElement, xterm: Xterm): { cols: number; rows: number } | null {
  const screen = element.querySelector<HTMLElement>(".xterm-screen");
  if (!screen) return null;

  const cell = { width: screen.clientWidth / xterm.cols, height: screen.clientHeight / xterm.rows };
  if (!(cell.width > 0) || !(cell.height > 0)) return null;

  const pane = element.getBoundingClientRect();
  if (!(pane.width > 0) || !(pane.height > 0)) return null;

  // The same floors and the same minimums as `FitAddon`: a terminal of zero
  // columns is not a smaller terminal, it is one that cannot be written to.
  return {
    cols: Math.max(2, Math.floor(pane.width / cell.width)),
    rows: Math.max(1, Math.floor(pane.height / cell.height)),
  };
}

export function TerminalPane({
  server,
  name,
  tmux,
}: {
  server: string;
  name: string;
  /// Which tmux session in the sandbox. `null` is the agent's own, which is
  /// what this meant before there were others.
  tmux: string | null;
}) {
  const host = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const element = host.current;
    if (!element) return;

    const xterm = new Xterm({
      convertEol: false,
      cursorBlink: true,
      // Ends in `monospace` so there is always something the platform can
      // resolve: xterm sizes its grid by measuring one character in this font,
      // and a family it cannot resolve measures zero.
      fontFamily: 'ui-monospace, "Cascadia Mono", Menlo, Consolas, monospace',
      fontSize: 13,
      // Taken from style.css, because the emulator paints its own background
      // and would otherwise sit as a rectangle of some other colour inside the
      // pane. See `palette`.
      theme: palette(THEME),
      // The sandbox's tmux keeps the scrollback that matters; this is just what
      // the pane can scroll back through without asking for it again.
      scrollback: 5000,
    });
    withUsableFontMetrics(element, () => xterm.open(element));

    const refit = () => {
      const size = gridFor(element, xterm);
      // A pane with no size yet -- a tab that is not on screen -- measures
      // nothing. Not a failure, and not worth resizing to.
      if (!size) return;
      if (size.cols !== xterm.cols || size.rows !== xterm.rows) {
        xterm.resize(size.cols, size.rows);
      }
    };

    // After the fonts and after a layout: `refit` measures the element, and
    // immediately after `open` the browser has laid out neither. Measuring
    // early leaves the default 80x24, which is then what the *server* sizes its
    // pty to -- the agent's screen comes back wrapped to eighty columns inside
    // a pane three times as wide.
    void document.fonts.ready.then(() => requestAnimationFrame(refit));

    const id = nextChannelId();
    let live = true;

    const sendSize = () => {
      // The server has no other way to know how wide this pane is: there is no
      // terminal on its side to take a size from, so the pty it allocates is
      // sized from here.
      terminal.resize(id, xterm.cols, xterm.rows).catch(() => {});
    };

    void open(server, id, { kind: "terminal", session: name, tmux }, (frame) => {
      if (!live) return;
      switch (frame.is) {
        case "opened":
          sendSize();
          break;
        case "output":
          xterm.write(decodeBytes(frame.data));
          break;
        case "closed":
          xterm.writeln(`\r\n\x1b[33m-- ${frame.reason ?? "detached"} --\x1b[0m`);
          break;
      }
    }).catch((e) => xterm.writeln(`\r\n\x1b[31m${String(e)}\x1b[0m`));

    const typed = xterm.onData((data) => {
      terminal.input(id, encodeBytes(new TextEncoder().encode(data))).catch(() => {});
    });

    // The pane resizes with the window and with the split; both go through the
    // observer rather than a window listener, which would miss the split.
    const observer = new ResizeObserver(refit);
    observer.observe(element);
    const resized = xterm.onResize(sendSize);

    return () => {
      live = false;
      observer.disconnect();
      typed.dispose();
      resized.dispose();
      void close(id);
      xterm.dispose();
    };
  }, [server, name, tmux]);

  return <div className="terminal" ref={host} />;
}
