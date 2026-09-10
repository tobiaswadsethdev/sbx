// The draggable edge between a sidebar and the middle.
//
// Two pixels of hit area over a one pixel border, which is the whole design:
// the line between the panes was already there and is what a person aims at, so
// the handle sits on top of it rather than adding a visible gutter of its own.
// A grabbable strip you can see is what makes an application look like a
// prototype of one.
//
// **Pointer capture, not a document listener.** `setPointerCapture` sends every
// move to this element until the button comes up, which is what makes a drag
// survive the pointer crossing the terminal in the middle -- an iframe or a
// canvas swallowing `mousemove` is the usual reason a hand-rolled splitter
// sticks. It also means there is nothing to clean up if the window loses focus
// mid-drag: the capture ends with the pointer.
//
// **And a keyboard.** A separator that only a mouse can move is a layout
// somebody using a keyboard cannot change at all, and this one has a real ARIA
// role with real values, so arrow keys move it and a screen reader can say how
// wide it is. Double-click resets, which is the one gesture people try on a
// splitter without being told.

import { useRef } from "react";

/// How far one arrow key moves the edge, and one with shift held.
const STEP = 16;
const BIG_STEP = 64;

export function Split({
  label,
  value,
  min,
  max,
  reset,
  /// Which way a rightward drag makes the pane bigger. The tree grows to the
  /// right of its handle and the dock grows to the left of its, so one of them
  /// has to invert -- and the caller is what knows which side it is on.
  grows,
  onChange,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  /// The width a double-click goes back to.
  reset: number;
  grows: "right" | "left";
  onChange: (width: number) => void;
}) {
  // The width and the pointer position the drag started from, so the pane
  // follows the total movement rather than accumulating per-event deltas --
  // which drifts, and drifts most on the frames a drag can least afford.
  const from = useRef<{ x: number; width: number } | null>(null);

  const sign = grows === "right" ? 1 : -1;
  const clamp = (width: number) => Math.min(max, Math.max(min, Math.round(width)));

  return (
    <div
      className="split"
      role="separator"
      // Vertical, because it separates panes side by side: the value moves
      // along the horizontal axis.
      aria-orientation="vertical"
      aria-label={label}
      aria-valuenow={value}
      aria-valuemin={min}
      aria-valuemax={max}
      // Focusable, or the keys below are unreachable.
      tabIndex={0}
      onPointerDown={(e) => {
        // Not a right-click, and not the second finger of a gesture.
        if (e.button !== 0) return;
        from.current = { x: e.clientX, width: value };
        e.currentTarget.setPointerCapture(e.pointerId);
        // Focus follows the grab, so releasing and reaching for an arrow key
        // continues the same adjustment.
        e.currentTarget.focus();
      }}
      onPointerMove={(e) => {
        const start = from.current;
        if (!start) return;
        // The pane is being sized, so a text selection started in the tree
        // behind the handle is not what the gesture means.
        e.preventDefault();
        onChange(clamp(start.width + sign * (e.clientX - start.x)));
      }}
      onPointerUp={() => {
        from.current = null;
      }}
      // A cancel is a gesture the browser took over -- a touch that became a
      // scroll. Ending the drag where it is beats leaving the handle armed.
      onPointerCancel={() => {
        from.current = null;
      }}
      onDoubleClick={() => onChange(reset)}
      onKeyDown={(e) => {
        // Left and right, and *not* inverted by `grows`: the pointer drags the
        // pane and the arrows move the separator, which is the thing they are
        // on. An arrow key that made the pane on the right grow when pressed
        // left would be the surprise.
        const step = e.shiftKey ? BIG_STEP : STEP;
        const by = e.key === "ArrowLeft" ? -step : e.key === "ArrowRight" ? step : 0;
        if (by !== 0) {
          e.preventDefault();
          onChange(clamp(value + sign * by));
          return;
        }
        if (e.key === "Home" || e.key === "End") {
          e.preventDefault();
          onChange(e.key === "Home" ? min : max);
          return;
        }
        // The keyboard's double-click.
        if (e.key === "Enter") {
          e.preventDefault();
          onChange(reset);
        }
      }}
    />
  );
}
