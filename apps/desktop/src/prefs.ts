// What this window remembers about itself.
//
// The other half of the settings screen, and the line between the two is not
// where the code happens to live -- it is who the answer belongs to. A branch
// prefix decides what every session on the server is called, including the ones
// `sbx new` starts, so it lives in the server's config file and this window
// only edits it. How wide the sidebars are is nobody's business but this
// window's, and putting it on the server would mean two people looking at the
// same sessions from two machines fighting over one number.
//
// `localStorage`, which for a Tauri window is a file in its own data directory
// -- so this survives the window closing without a request, a round trip or a
// file format. Every value is read through a validator on the way out: the
// store is on disk, an old build may have written something else into it, and a
// width of `null` is a sidebar with no width at all.

import { useCallback, useEffect, useState } from "react";

/// One key, so a stray `sbx.theme` from some future build cannot be mistaken
/// for part of this object.
const KEY = "sbx.prefs";

export type Prefs = {
  /// The sidebars, in pixels. Also what dragging their edges writes -- there is
  /// one number and two ways to set it, rather than a drag the settings screen
  /// disagrees with.
  treeWidth: number;
  dockWidth: number;
  /// How often the worktree list is re-read, in milliseconds. A round trip to a
  /// server that may be a continent away, which is the reason this is worth a
  /// preference at all: three seconds is right on a LAN and wrong over a VPN.
  refreshMs: number;
  /// Whether the OS is told when an agent starts waiting.
  ///
  /// On by default, because it is the largest thing this window has over a
  /// terminal. Off is for somebody who keeps it on screen anyway and does not
  /// want their notification centre to hold a record of it.
  notify: boolean;
};

/// The bounds, and the reasons.
///
/// Named rather than inlined because the drag handles and the settings screen
/// both clamp to them, and a drag that could reach a width the settings screen
/// refuses would be two answers to how narrow a sidebar may be.
export const LIMITS = {
  // 180 is where a worktree card stops being able to show a name and a branch
  // on two lines; past 560 the tree is wider than the diff beside it.
  treeWidth: { min: 180, max: 560 },
  // 280 fits the file tree's deepest realistic path; 900 is where the middle
  // stops being where you work.
  dockWidth: { min: 280, max: 900 },
  // The ceiling is the config file's own -- past a minute the list has stopped
  // being live and the window would be better closed. The floor is *not*: the
  // file allows 250ms because there it scaled an exec inside a sandbox on this
  // machine, and this is two requests to a server that may be a continent
  // away. Four `Ls` a second down a VPN is a preference nobody should be able
  // to set by holding an arrow key.
  refreshMs: { min: 500, max: 60_000 },
} as const;

export const DEFAULTS: Prefs = {
  // The widths the window shipped with, so an existing install looks the same
  // the first time it runs a build that can change them.
  treeWidth: 260,
  dockWidth: 400,
  refreshMs: 3000,
  notify: true,
};

function clamp(value: number, { min, max }: { min: number; max: number }): number {
  return Math.min(max, Math.max(min, Math.round(value)));
}

/// Anything on disk, as `Prefs`.
///
/// Field by field rather than a spread over the defaults: a stored `treeWidth`
/// of `"260"` or of `NaN` would survive the spread and reach a CSS width, and
/// the whole reason the store is validated is that this build is not
/// necessarily the one that wrote it.
export function sanitize(stored: unknown): Prefs {
  const raw = (typeof stored === "object" && stored !== null ? stored : {}) as Record<
    string,
    unknown
  >;
  const number = (value: unknown, limit: { min: number; max: number }, fallback: number) =>
    typeof value === "number" && Number.isFinite(value) ? clamp(value, limit) : fallback;
  return {
    treeWidth: number(raw.treeWidth, LIMITS.treeWidth, DEFAULTS.treeWidth),
    dockWidth: number(raw.dockWidth, LIMITS.dockWidth, DEFAULTS.dockWidth),
    refreshMs: number(raw.refreshMs, LIMITS.refreshMs, DEFAULTS.refreshMs),
    notify: typeof raw.notify === "boolean" ? raw.notify : DEFAULTS.notify,
  };
}

function read(): Prefs {
  try {
    const stored = window.localStorage.getItem(KEY);
    return sanitize(stored === null ? null : JSON.parse(stored));
  } catch {
    // A quota, a private mode, or a value that is not JSON. The defaults are
    // meant to be good, and a window that will not open because it could not
    // remember how wide a sidebar was would be the worse failure.
    return DEFAULTS;
  }
}

/// The prefs, and a setter that persists.
///
/// One hook used in one place -- `App` -- and passed down, rather than a store
/// each component reads: the widths are laid out by the grid in `App`, and two
/// components holding their own copy is how a drag and the settings screen
/// start disagreeing.
export function usePrefs(): [Prefs, (change: Partial<Prefs>) => void] {
  const [prefs, setPrefs] = useState<Prefs>(read);

  const update = useCallback((change: Partial<Prefs>) => {
    setPrefs((current) => sanitize({ ...current, ...change }));
  }, []);

  // Written in an effect rather than in the setter, so a drag that fires forty
  // updates a second does forty state changes and one write per frame at most
  // -- and so the write is skipped entirely when React coalesced them.
  useEffect(() => {
    try {
      window.localStorage.setItem(KEY, JSON.stringify(prefs));
    } catch {
      // Nothing to say: the window works, it just will not remember.
    }
  }, [prefs]);

  return [prefs, update];
}
