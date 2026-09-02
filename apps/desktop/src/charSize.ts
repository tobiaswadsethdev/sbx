// Making a character measurable, and putting it where it belongs, on an engine
// where neither is true by default.
//
// Two faults, one cause: WebKitGTK reports a font's vertical metrics wrongly,
// and xterm believes them. Both are corrected here, under one probe, because
// the second is only reachable once the first is fixed -- a terminal that never
// draws cannot be seen to draw its rows two pixels too high.
//
// **The cell has no height.** xterm sizes its grid by measuring one character,
// and everything downstream is that measurement: the renderer refuses to draw a
// zero-sized cell, `FitAddon` returns `undefined` rather than a column count,
// and the pane stays empty with the agent's screen sitting in the buffer
// behind it. `CharSizeService` measures with an `OffscreenCanvas` where it can
// and falls back to measuring a DOM element where it cannot -- but the test it
// applies is whether `fontBoundingBoxAscent` and `fontBoundingBoxDescent`
// *exist*. In WebKitGTK they exist and are always zero, for an ordinary canvas
// as much as an offscreen one, so the height comes out zero and the fallback is
// never reached. Zero is then read as "the element is `display:none`, keep the
// value from last time", and there is no value from last time.
//
// **The rows are then drawn too high.** The DOM fallback measures a span whose
// `line-height` is `normal`, so the height it returns *is* the font's natural
// line box. xterm takes that number and renders each row with
// `line-height: <that>px` -- the same number, and in WebKit not the same thing.
// Measured in 2.52.6, at 13px, in the font every monospace family on this
// machine resolves to:
//
//     line-height: normal  ->  baseline 13px from the top of the row
//     line-height: 17px    ->  baseline  8px from the top of the row
//
// Capitals ink 10px above the baseline and brackets 12px, and rows are
// `overflow: hidden`, so at a baseline of 8 the top two pixels of every line
// are sheared off -- `README` reads as `KEADME`. Raising xterm's `lineHeight`
// only moves the baseline by half of what it adds, and could not lift brackets
// clear at any usable row height; restoring `line-height: normal` on the row's
// spans puts the baseline back at 13 and costs nothing. That is not a nudge
// until it looks right: the cell height was *measured* as the natural line box,
// so rendering at the natural line box is the only setting consistent with it.
// It would stop being so if xterm's `lineHeight` option were ever moved off 1,
// which is why nothing here does.
//
// On an engine whose font metrics work -- WebView2, and every Chromium -- the
// probe passes and none of this applies.

/// Marks a terminal whose engine could not measure a font. `style.css` hangs
/// the line-height correction off this.
const UNTRUSTED = "broken-font-metrics";

/// Whether this engine's canvas can measure a font's height. Probed once: it is
/// a property of the build, and it cannot change under us.
let canMeasureHeight: boolean | undefined;

function canvasMeasuresHeight(): boolean {
  if (canMeasureHeight === undefined) {
    canMeasureHeight = probe();
  }
  return canMeasureHeight;
}

function probe(): boolean {
  try {
    const context = new OffscreenCanvas(2, 2).getContext("2d");
    if (!context) return true;
    context.font = "13px monospace";
    const metrics = context.measureText("W");
    return metrics.fontBoundingBoxAscent + metrics.fontBoundingBoxDescent > 0;
  } catch {
    // No `OffscreenCanvas`, or no 2d context from one. xterm's own check throws
    // on the same thing and takes the DOM path unaided; there is nothing to do.
    return true;
  }
}

/// Run `open` -- `Terminal.open`, or anything that constructs a `Terminal`'s
/// browser services -- against `host`, with a character cell that can be
/// measured and a baseline that lands where the measurement says it should.
export function withUsableFontMetrics<T>(host: HTMLElement, open: () => T): T {
  if (typeof OffscreenCanvas === "undefined" || canvasMeasuresHeight()) {
    return open();
  }

  host.classList.add(UNTRUSTED);

  // Restored by descriptor rather than by assignment: the global is
  // non-enumerable, and putting back an enumerable one would leave
  // `OffscreenCanvas` showing up in every enumeration of `window` for the life
  // of the process. A small thing to get wrong invisibly.
  const original = Object.getOwnPropertyDescriptor(globalThis, "OffscreenCanvas");
  delete (globalThis as { OffscreenCanvas?: unknown }).OffscreenCanvas;
  try {
    return open();
  } finally {
    if (original) {
      Object.defineProperty(globalThis, "OffscreenCanvas", original);
    }
  }
}
