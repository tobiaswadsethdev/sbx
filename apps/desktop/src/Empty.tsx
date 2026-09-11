// Nothing here, and nothing loaded yet: the two things every pane in this
// window has to be able to say.
//
// They used to be sentences, one per pane, each written by whoever built that
// pane: "nothing changed", "no policy decisions in the recent log", "Nothing
// assigned to you — or no trackers yet, which is what the integrations screen
// adds." Twelve of them, in twelve voices, and the cost was not the words --
// it was that none of them looked like the others. A pane you have not seen
// before greeted you with a paragraph to read before you could tell whether
// anything was wrong.
//
// A glyph is the better answer, and for a reason particular to absence rather
// than a preference for pictures: **an empty pane has no content to compete
// with**, so the mark can be large enough to be read at a glance from across
// the window, which is exactly how these are read. What you want to know is
// "is this pane empty, or broken, or still working?" -- three states, and one
// look should settle it. A centred glyph in `--dimmer` is empty, a spinner is
// working, red text is broken. No reading involved.
//
// **The note is optional and is not a replacement for the glyph.** Where it
// survives it is there because the absence has a *cause someone can act on* --
// "no trackers yet" is a thing to go and fix, and "no changes" is not -- and it
// is a fragment rather than a sentence, because anything longer is a paragraph
// that has grown back. The glyph says what is empty; the note says what to do
// about it, and most absences have nothing to say.

import { ICON_BIG, ICON_PAGE, ICON_SIZE } from "./icons";

/// How much room the absence has, which decides how big its mark is.
///
/// Not a style choice per caller: the three sizes correspond to the three
/// places a nothing can appear, and naming them here is what stops a fourth
/// from being invented inline. `row` is an absence *inside* a list -- an empty
/// project group, a directory with no files -- and sits on the baseline of the
/// rows around it. `pane` is a dock pane or a section of a screen with nothing
/// in it. `page` is the whole middle of the window.
type Size = "row" | "pane" | "page";

const MARK: Record<Size, number> = {
  row: ICON_SIZE,
  pane: ICON_BIG,
  page: ICON_PAGE,
};

/// A glyph standing in for whatever is not here.
///
/// `icon` is one of the absences named in `icons.tsx`, and taking it as a prop
/// rather than deriving it from a `kind` string is the point: the glyph is the
/// pane's *own* subject gone quiet -- a tick for a clean working copy, an
/// unplugged plug for no integrations -- and only the caller knows what its
/// subject is.
export function Empty({
  icon: Mark,
  note,
  size = "pane",
  tone,
  children,
}: {
  icon: React.ComponentType<{ size?: number; className?: string }>;
  /// A fragment, not a sentence, and only where the absence is actionable.
  /// See the note at the top of this file.
  note?: string;
  size?: Size;
  /// `"ok"` for the one absence in the window that is an answer rather than a
  /// shortfall: a working copy with nothing changed.
  ///
  /// A prop and not a rule in the stylesheet, because CSS cannot tell which
  /// glyph it has been handed -- and it exists at all only so that the tick
  /// can wear `--ok` while every other mark here stays grey. There is
  /// deliberately no `"bad"` beside it: a pane that failed to load is an
  /// error, and errors are red text, not a grey glyph in a different colour.
  tone?: "ok";
  /// What to do about it: a button, or the two commands a first run needs.
  /// Rendered under the note, and the reason this is a component with children
  /// rather than a function returning a glyph.
  children?: React.ReactNode;
}) {
  return (
    <div className={`empty ${size}${tone ? ` ${tone}` : ""}`}>
      {/* `size` as a prop rather than a stylesheet rule, because scaling one
          of these in CSS changes its stroke weight -- see `ICON_BIG`. */}
      <Mark size={MARK[size]} className="empty-mark" />
      {note && <p className="empty-note">{note}</p>}
      {children}
    </div>
  );
}

/// Still asking. The same spinner the worktree list uses for a working agent.
///
/// Deliberately the same mark, and deliberately without words. Every pane in
/// the window used to name what it was waiting for -- "reading git…", "asking
/// the trackers…", "reading the options…" -- and a label on a wait is only
/// worth its space if the wait is long enough to read it *and* the thing being
/// waited for is in doubt. Neither holds here: these are one request each to a
/// server that is either there or is about to say it is not, and the pane you
/// just opened is the thing being read.
///
/// The one case that keeps its words is a wait that is *not* a fetch -- an
/// install, an upload -- where the label is the only thing saying that
/// something irreversible is in progress.
///
/// It draws its own spinner rather than reusing `StateDot`'s, and the
/// difference is the colour rather than the shape. The four state hues mean
/// "an agent is working", "an agent wants you", "it passed", "it failed", and
/// nothing else in the window may wear one -- see the top of `style.css`. A
/// pane fetching its own contents is not an agent doing anything, so this is
/// the neutral one. The animation is shared, because the *motion* carries no
/// such claim.
export function Waiting({ size = "pane" }: { size?: Size }) {
  return (
    <div className={`empty ${size}`} role="status" aria-label="loading">
      <span className="spin" />
    </div>
  );
}
