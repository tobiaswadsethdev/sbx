// The icons: lucide, pinned to one grid, plus the file-kind glyphs it has no
// equivalent for.
//
// This file used to argue against an icon set, and the argument was really
// about consistency rather than about packages: a set arrives with its own idea
// of optical size and stroke weight, and fifteen icons from it beside fifteen
// drawn here would read as two families. `LucideProvider` in `main.tsx`
// settles that centrally -- every lucide icon in the window renders at one size
// and one stroke, whatever the library's own defaults are -- so the objection
// is answered rather than accepted. What is left of the old argument is the
// bottom half of this file, which stays hand-drawn because it has to.
//
// `absoluteStrokeWidth` is the part that makes the pinning work. lucide draws
// on a 24-grid and scales the stroke with the icon, so one `strokeWidth` at
// 14px and at 20px are two different weights on screen; with it set, the number
// *is* the rendered width in pixels, and an icon can be resized without
// changing weight. See `ICON_STROKE` below.
//
// Monaco does bundle codicons, and reusing them was still the wrong
// alternative: it would tie the window's chrome to a version of an editor it
// happens to embed, and the file tree would change shape the day Monaco is
// swapped.

import {
  ChevronDown,
  ChevronRight,
  CircleAlert,
  CircleCheck,
  CircleQuestionMark,
  Folder as FolderClosed,
  FolderOpen,
  FolderPlus,
  GitBranch,
  Inbox as InboxGlyph,
  Plug,
  Plus as PlusGlyph,
  Minus as MinusGlyph,
  RefreshCw,
  Server as ServerGlyph,
  ShieldOff,
  Trash,
  Undo2,
  X,
} from "lucide-react";

import type { State } from "./gen/State";

/// The grid every icon in this window is on: 14 pixels across, with a stroke
/// of `ICON_STROKE` *actual* pixels. Applied to lucide through its provider in
/// `main.tsx` and to the hand-drawn glyphs below by hand, which is the whole
/// reason both numbers are named here rather than written twice.
export const ICON_SIZE = 14;
/// 1.25 rather than lucide's 2. The window's text is 12 and 13 pixels, and a
/// two-pixel stroke beside it reads as bold -- which is what an icon set at its
/// default weight looks like dropped into an interface built at this scale.
export const ICON_STROKE = 1.25;

// The chrome, renamed for what it does here rather than what lucide calls it.
// A rename per icon is worth it: `Forget` says which button it is on and
// `Trash` does not, and the day one is swapped for a better glyph the change is
// one line in this file instead of a find-and-replace across the app.
export const Plus = PlusGlyph;
export const Minus = MinusGlyph;
export const Close = X;
export const Revert = Undo2;
export const Refresh = RefreshCw;
export const Branch = GitBranch;
export const Inbox = InboxGlyph;
export const Integrations = Plug;
export const Servers = ServerGlyph;
export const NewProject = FolderPlus;
export const Forget = Trash;
/// A session with no sandbox around it. There is no `Sandboxed` beside it on
/// purpose: sandboxed is what every session is, and a mark on the rule as well
/// as on the exception is a mark that says nothing. See `Tree.tsx`.
export const Unsandboxed = ShieldOff;

type Props = { className?: string; title?: string };

/// A directory, open or shut. Two glyphs behind one prop, because the caller
/// has a boolean and not a choice of icon.
export const Folder = ({ open, ...p }: Props & { open: boolean }) =>
  open ? <FolderOpen {...p} /> : <FolderClosed {...p} />;

/// The file tree's twisty. Down when expanded, right when not -- the rotation
/// is two glyphs rather than a CSS transform so the stroke ends stay on the
/// pixel grid at 14px.
export const Chevron = ({ open, ...p }: Props & { open: boolean }) =>
  open ? <ChevronDown {...p} /> : <ChevronRight {...p} />;

/// What the agent in a session is doing, as one fixed-size mark.
///
/// Fixed-size is the requirement, not a detail: this sits in a column to the
/// left of every worktree's name, and a mark that changed size with the state
/// would shuffle the name of every row each time an agent started or stopped.
/// Every branch below therefore renders into the same 14-pixel box.
///
/// The colours are in `style.css`, keyed on the state name, for the same reason
/// the palette is: one place to change what `waiting` looks like.
export function StateDot({ state, className }: { state: State; className?: string }) {
  const box = `state-dot ${state} ${className ?? ""}`;

  switch (state) {
    // In progress, and the two are worth telling apart: `running` is an agent
    // working, `creating`/`seeding` is the sandbox not being there yet. Same
    // spinner, different hue, because the thing you do about them is the same
    // -- wait -- and the thing they mean is not.
    case "running":
    case "creating":
    case "seeding":
      return (
        <span className={box} role="img" aria-label={state}>
          <span className="spinner" />
        </span>
      );

    // The one state the window exists to tell you about, so it gets a glyph
    // rather than a dot: a shape is findable in a list of twelve rows in a way
    // that a colour is not, and it is the row you are *not* looking at.
    case "waiting":
      return (
        <span className={box} role="img" aria-label="waiting for input">
          <CircleQuestionMark />
        </span>
      );

    case "published":
      return (
        <span className={box} role="img" aria-label="published">
          <CircleCheck />
        </span>
      );

    case "failed":
    case "dead":
      return (
        <span className={box} role="img" aria-label={state}>
          <CircleAlert />
        </span>
      );

    // Healthy and doing nothing. A plain dot, and deliberately the quietest
    // mark here: it is what most rows are most of the time, and a list where
    // every row draws attention has none left for the row that should.
    default:
      return (
        <span className={box} role="img" aria-label={state}>
          <span className="dot" />
        </span>
      );
  }
}

// ---------------------------------------------------------------------------
// The file-kind glyphs, still drawn by hand.
//
// Not stubbornness: what these encode is "rust", "lock file", "config", which
// is a judgement about a filename rather than a picture, and no set ships it.
// lucide has a page and a folder; it does not have "this is the lock file, do
// not read it". They are on the same 14-pixel grid and the same stroke as
// everything above, taken from the constants rather than repeated, so a page
// from here beside a chevron from lucide is one family.
// ---------------------------------------------------------------------------

/// The frame the glyphs below are drawn in: a 16 viewBox rendered at
/// `ICON_SIZE`, with the stroke pre-divided so it lands on `ICON_STROKE`
/// actual pixels -- the same arithmetic lucide's `absoluteStrokeWidth` does.
function Svg({ children, className, title }: Props & { children: React.ReactNode }) {
  return (
    <svg
      className={`lucide ${className ?? ""}`}
      viewBox="0 0 16 16"
      width={ICON_SIZE}
      height={ICON_SIZE}
      fill="none"
      stroke="currentColor"
      strokeWidth={(ICON_STROKE * 16) / ICON_SIZE}
      strokeLinecap="round"
      strokeLinejoin="round"
      // Decorative by default: the filename beside it is the name. A title is
      // set only where the icon is the whole control.
      aria-hidden={title ? undefined : true}
      role={title ? "img" : undefined}
    >
      {title && <title>{title}</title>}
      {children}
    </svg>
  );
}

/// A page with a folded corner. The base every file icon is drawn on, so an
/// unknown extension is the same shape as a known one rather than nothing.
const Page = ({ children }: { children?: React.ReactNode }) => (
  <>
    <path d="M9 1.8H4.5a1 1 0 0 0-1 1v10.4a1 1 0 0 0 1 1h7a1 1 0 0 0 1-1V5.3z" />
    <path d="M9 1.8v3.5h3.5" />
    {children}
  </>
);

export const File = (p: Props) => (
  <Svg {...p}>
    <Page />
  </Svg>
);

/// The extensions worth telling apart at a glance, and nothing else.
///
/// A short list on purpose. Two hundred entries is two hundred chances to be
/// subtly wrong, and the value of a file icon is almost entirely in the few
/// kinds you scan a directory for -- source, config, docs, lock files. The rest
/// get the page, which is honest.
const KIND: Record<string, string> = {
  rs: "rust",
  ts: "code",
  tsx: "code",
  js: "code",
  jsx: "code",
  py: "code",
  go: "code",
  cs: "code",
  java: "code",
  c: "code",
  h: "code",
  cpp: "code",
  sh: "shell",
  bash: "shell",
  json: "config",
  toml: "config",
  yaml: "config",
  yml: "config",
  ini: "config",
  conf: "config",
  lock: "lock",
  md: "doc",
  txt: "doc",
  css: "style",
  html: "style",
  png: "image",
  jpg: "image",
  jpeg: "image",
  svg: "image",
  gif: "image",
  webp: "image",
  ico: "image",
};

export function kindOf(name: string): string {
  const lower = name.toLowerCase();
  if (lower === "cargo.lock" || lower === "package-lock.json") return "lock";
  if (lower.startsWith(".git")) return "config";
  const ext = lower.includes(".") ? (lower.split(".").pop() ?? "") : "";
  return KIND[ext] ?? "plain";
}

/// A file's icon, by what it is.
export function FileIcon({ name, className }: { name: string; className?: string }) {
  const kind = kindOf(name);
  return (
    <Svg className={`${className ?? ""} kind-${kind}`}>
      <Page>
        {kind === "code" && <path d="M6.6 8.6L5.3 10l1.3 1.4M9.4 8.6L10.7 10l-1.3 1.4" />}
        {kind === "rust" && <path d="M6 11.6V8.4h2a1.1 1.1 0 0 1 0 2.2H6.6l1.9 1" />}
        {kind === "shell" && <path d="M5.4 8.8l1.7 1.4-1.7 1.4M8.6 11.8h2.2" />}
        {kind === "config" && (
          <>
            <circle cx="8" cy="10.4" r="1.5" />
            <path d="M8 7.9v.6M8 12.3v.6M5.9 9.2l.5.3M9.6 11.3l.5.3M5.9 11.6l.5-.3M9.6 9.5l.5-.3" />
          </>
        )}
        {kind === "lock" && (
          <>
            <rect x="5.7" y="10" width="4.6" height="3.2" rx="0.6" />
            <path d="M6.9 10V9.1a1.1 1.1 0 0 1 2.2 0V10" />
          </>
        )}
        {kind === "doc" && <path d="M5.6 8.8h4.8M5.6 10.8h4.8M5.6 12.6h3" />}
        {kind === "style" && <path d="M8 7.8l2.4 1.5v2.9L8 13.7l-2.4-1.5V9.3z" />}
        {kind === "image" && (
          <>
            <circle cx="6.5" cy="9.3" r="0.9" />
            <path d="M4.4 13l2.4-2.4 1.5 1.5 1.4-1.4 2 2" />
          </>
        )}
      </Page>
    </Svg>
  );
}
