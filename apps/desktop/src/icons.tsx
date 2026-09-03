// The icons, as inline SVG.
//
// Drawn here rather than pulled in, for the reason most of this repository
// avoids a dependency: an icon set is a font or a package of a thousand glyphs
// to use fifteen, and each one arrives with its own idea of stroke weight and
// optical size. These are one grid, one stroke, and they inherit `currentColor`
// so a state colour on the parent is the icon's colour too.
//
// Monaco does bundle codicons, and reusing them was the obvious alternative.
// It would tie the window's chrome to a version of an editor it happens to
// embed, and the file tree would change shape the day Monaco is swapped.

type Props = { className?: string; title?: string };

/// One 16-grid, 1.4 stroke, round caps. Every icon below is this frame.
function Svg({ children, className, title }: Props & { children: React.ReactNode }) {
  return (
    <svg
      className={`icon ${className ?? ""}`}
      viewBox="0 0 16 16"
      width="14"
      height="14"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.4"
      strokeLinecap="round"
      strokeLinejoin="round"
      // Decorative by default: the label beside it is the name. A title is set
      // only where the icon is the whole control.
      aria-hidden={title ? undefined : true}
      role={title ? "img" : undefined}
    >
      {title && <title>{title}</title>}
      {children}
    </svg>
  );
}

export const Plus = (p: Props) => (
  <Svg {...p}>
    <path d="M8 3.5v9M3.5 8h9" />
  </Svg>
);

export const Minus = (p: Props) => (
  <Svg {...p}>
    <path d="M3.5 8h9" />
  </Svg>
);

export const Close = (p: Props) => (
  <Svg {...p}>
    <path d="M4 4l8 8M12 4l-8 8" />
  </Svg>
);

/// Discard: an arrow going back on itself.
export const Revert = (p: Props) => (
  <Svg {...p}>
    <path d="M3 8a5 5 0 1 1 1.7 3.8" />
    <path d="M3 4.5V8h3.5" />
  </Svg>
);

export const Refresh = (p: Props) => (
  <Svg {...p}>
    <path d="M13 8a5 5 0 1 1-1.7-3.8" />
    <path d="M13 3v3.5H9.5" />
  </Svg>
);

export const Chevron = ({ open, ...p }: Props & { open: boolean }) => (
  <Svg {...p}>{open ? <path d="M4 6.5l4 4 4-4" /> : <path d="M6.5 4l4 4-4 4" />}</Svg>
);

export const Folder = ({ open, ...p }: Props & { open: boolean }) =>
  open ? (
    <Svg {...p}>
      <path d="M2 12.5V4a1 1 0 0 1 1-1h3l1.5 2H12a1 1 0 0 1 1 1v1" />
      <path d="M2 12.5L3.8 7.5H14.5L12.7 12.5z" />
    </Svg>
  ) : (
    <Svg {...p}>
      <path d="M2 12.5V4a1 1 0 0 1 1-1h3l1.5 2H13a1 1 0 0 1 1 1v6.5a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1z" />
    </Svg>
  );

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
