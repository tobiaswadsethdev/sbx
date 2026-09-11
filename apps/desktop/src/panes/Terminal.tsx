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
import { Terminal as Xterm, type ITheme } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

import { withUsableFontMetrics } from "../charSize";
import { close, decodeBytes, encodeBytes, nextChannelId, open, terminal } from "../stream";

/// Which custom property each of xterm's colours comes from, and what to use if
/// the stylesheet has not loaded. The emulator paints its own surface, so these
/// have to agree with `style.css` -- and the fallbacks are only for the case
/// where there is nothing to agree with.
const PALETTE = {
  background: ["--sunken", "#0a0a0a"],
  foreground: ["--text", "#fafafa"],
  // No accent to borrow: the window's hues mean "working", "needs you", "good"
  // and "bad", and a cursor is none of those. Near-white on near-black, which
  // is how everything else in the window takes emphasis.
  cursor: ["--text", "#fafafa"],
  scrollbarSliderBackground: ["--line-strong", "rgba(255, 255, 255, 0.15)"],
  scrollbarSliderHoverBackground: ["--dim", "#a1a1a1"],
  scrollbarSliderActiveBackground: ["--dim", "#a1a1a1"],
} satisfies Record<string, [string, string]>;

/// The terminal's colours, read from `style.css` rather than written down here.
///
/// They *were* written down here, and then the palette moved out from under
/// them: `--bg-sunken: #0e0e12` became `--sunken: #0a0a0a`, and the literal
/// stayed. Every terminal in the window sat as a faintly blue rectangle inside
/// a frame of a black belonging to no palette at all. A custom property cannot
/// drift the way a copy of one can.
///
/// Read as an element's resolved `color` rather than straight off the property,
/// because the property's *value* is whatever style.css wrote -- the lines are
/// `rgb(255 255 255 / 0.15)`, modern space-separated syntax -- and xterm parses
/// `#rgb[a]`, `#rrggbb[aa]`, `rgb()` and `rgba()` and *throws* on anything else
/// it cannot round-trip through a canvas opaquely. Resolving the property as a
/// colour hands back the serialised form, which is always one xterm accepts.
function palette(): ITheme {
  const probe = document.createElement("span");
  probe.style.display = "none";
  document.body.appendChild(probe);
  try {
    return Object.fromEntries(
      Object.entries(PALETTE).map(([key, [property, fallback]]) => {
        probe.style.color = `var(${property}, ${fallback})`;
        return [key, getComputedStyle(probe).color];
      }),
    );
  } finally {
    probe.remove();
  }
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
      theme: palette(),
      // The sandbox's tmux keeps the scrollback that matters; this is just what
      // the pane can scroll back through without asking for it again.
      scrollback: 5000,
    });
    const fit = new FitAddon();
    xterm.loadAddon(fit);
    withUsableFontMetrics(element, () => xterm.open(element));

    const refit = () => {
      try {
        fit.fit();
      } catch {
        // A pane with no size yet -- a tab that is not on screen -- throws
        // rather than returning. Not a failure worth showing.
      }
    };

    // After the fonts and after a layout: `fit` measures the element, and
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
