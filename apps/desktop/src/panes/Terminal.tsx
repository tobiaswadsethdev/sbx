// The agent's screen, live.
//
// xterm.js over the terminal channel: bytes out of the sandbox's tmux into the
// emulator, keystrokes back. Nothing here interprets the stream -- the escape
// sequences are the agent's, and the one thing this must not do is try to
// understand them.
//
// **This does not paint under WSLg**, and the cause is not in this file or
// anywhere below it. See docs/desktop.md: the bytes arrive and reach xterm's
// buffer, and its renderer never draws them because the character cell measures
// zero. Verified by writing a literal string with no stream involved, which
// does not appear either.

import { useEffect, useRef } from "react";
import { Terminal as Xterm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

import { close, decodeBytes, encodeBytes, nextChannelId, open, terminal } from "../stream";

export function TerminalPane({ server, name }: { server: string; name: string }) {
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
      // Matching style.css, because the emulator paints its own background and
      // would otherwise sit as a black rectangle inside a dark grey pane.
      theme: { background: "#0e0e12", foreground: "#d6d6dd", cursor: "#e9c46a" },
      // The sandbox's tmux keeps the scrollback that matters; this is just what
      // the pane can scroll back through without asking for it again.
      scrollback: 5000,
    });
    const fit = new FitAddon();
    xterm.loadAddon(fit);
    xterm.open(element);

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

    void open(server, id, { kind: "terminal", session: name }, (frame) => {
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
  }, [server, name]);

  return <div className="terminal" ref={host} />;
}
