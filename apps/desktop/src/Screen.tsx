// A screen: one of the window's destinations, filling the middle instead of
// floating over it.
//
// The inbox, the integrations, the servers and the settings were four modal
// dialogs, and being modal was wrong for all four in the same way. A dialog is
// the right shape for a *question* -- it takes the window hostage because it
// needs one answer before anything else can happen, which is exactly what
// `Confirm.tsx` does and why that one is still a dialog. None of these four
// asks anything. They are places you go to look at a list and change something
// in it, and a place is a screen.
//
// What being modal actually cost:
//
//   - **Room.** The integrations screen is a container per row with its own log
//     under it, and it was living in a 760-pixel box with `max-height: 82vh`
//     and a scrollbar of its own inside a window that already had one. The
//     scrim around it was reserving a third of a 1600-pixel display to draw
//     black over.
//   - **A scrim over the thing you were reading.** These screens explain the
//     workspace behind them -- a tracker with no credential is why the inbox is
//     empty -- and the modal dimmed the evidence while you read about it.
//   - **Nowhere to be.** A dialog has no address. Two of them could not be open
//     at once, going from the inbox to the create form meant one closing and
//     another opening over the top, and nothing in the window said where you
//     were. The header's icons now do: the one you are on is lit.
//
// The create forms stayed dialogs, and that is not an oversight either --
// `NewProject` and `NewWorktree` are questions with a submit button, asked from
// somewhere and answered back to it.

import { Close } from "./icons";

export function Screen({
  icon: Mark,
  title,
  children,
  onClose,
  actions,
}: {
  /// The same glyph as the header button that opens this screen. The pairing is
  /// the whole navigation model: the icon in the strip is lit, and the icon on
  /// the page it opened is the same picture, so an icon-only header is
  /// learnable by using it rather than by hovering everything once.
  icon: React.ComponentType<{ className?: string }>;
  title: string;
  /// Whatever this screen does that is not about one row -- pushing skills,
  /// saving settings. To the left of the close button, which is always last.
  actions?: React.ReactNode;
  onClose: () => void;
  children: React.ReactNode;
}) {
  return (
    <section className="screen">
      {/* The title survives the cull, and it is the one label in the window
          that earns its place by being somewhere you *arrive*: the header icon
          says where you are only if you already know what the icon means, and
          this is where you find out. One word each, which is as much as any of
          them needs. */}
      <header className="screen-head">
        <Mark className="screen-mark" />
        <h1>{title}</h1>
        {actions}
        <button className="quiet-icon" title="back to the workspace" onClick={onClose}>
          <Close aria-label="close" />
        </button>
      </header>
      {/* Escape closes it, and the listener is in `App` rather than here --
          one screen is open at a time and the app is what knows which. Four
          copies of the same key handler is how two of them end up disagreeing
          about what Escape does. */}
      <div className="screen-body scrollbar-sleek">
        <div className="screen-inner">{children}</div>
      </div>
    </section>
  );
}
