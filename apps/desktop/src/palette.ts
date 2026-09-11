// The window's colours, for the two panes that paint their own.
//
// The terminal and the editor are not styled by `style.css`: xterm and Monaco
// each take a *theme* of literal colour values and paint their surface
// themselves, and neither will resolve a `var(--sunken)` handed to it. So
// something has to carry the palette across, and for a long time that something
// was three hex literals copied into `Terminal.tsx` and two into `File.tsx`.
//
// They were right when they were written, and then the palette was replaced
// wholesale: `--bg-sunken: #0e0e12` became `--sunken: #0a0a0a`, `--text` went
// from `#d6d6dd` to `#fafafa`, and the amber accent was removed on purpose. The
// literals stayed, under comments saying they matched. Both panes sat as
// faintly blue rectangles inside a window of a different black -- the terminal
// framed in `#000` on top of that, and the file pane rounded off at the corners
// so the seam had a shape.
//
// Copying at runtime instead. It is still a copy -- neither library can be
// given a custom property -- but it is one made from the stylesheet each time
// the window opens rather than by hand once, and a copy that is remade cannot
// go stale.

/// Resolve custom properties to colours the two libraries will accept.
///
/// Read as an element's computed `color` rather than straight off the property.
/// The property's *value* is whatever `style.css` wrote, and this file's lines
/// are `rgb(255 255 255 / 0.15)` -- modern space-separated syntax, which neither
/// library parses. Resolving it as a colour hands back the engine's
/// serialisation instead, which is `rgb()` or `rgba()` and is at least a form
/// with a grammar either one could have been written against.
///
/// Neither was. It is handed back as `#rrggbb[aa]`, because that is the only
/// notation *both* take: xterm documents it as the fastest it parses, and Monaco
/// takes nothing else -- `rgb(10, 10, 10)` reaches it as
/// `Illegal value for token color`, thrown out of `defineTheme` and up through
/// the pane, so the file tab renders as a React error boundary rather than an
/// editor. Found by opening a file in the built application; a browser has
/// Monaco no more than it has WebKitGTK.
///
/// The fallbacks are not decoration. Module evaluation order puts `App` -- and
/// so both panes -- ahead of `import "./style.css"` in `main.tsx`, so anything
/// reading the palette at module scope reads it before there is one. Panes call
/// this from an effect, by which time the stylesheet is in; the fallbacks cover
/// the case where something later does not.
export function palette<K extends string>(wanted: Record<K, [string, string]>): Record<K, string> {
  const probe = document.createElement("span");
  probe.style.display = "none";
  document.body.appendChild(probe);
  try {
    return Object.fromEntries(
      Object.entries<[string, string]>(wanted).map(([key, [property, fallback]]) => {
        probe.style.color = `var(${property}, ${fallback})`;
        return [key, hex(getComputedStyle(probe).color)];
      }),
    ) as Record<K, string>;
  } finally {
    probe.remove();
  }
}

/// `rgb(10, 10, 10)` or `rgba(255, 255, 255, 0.15)` as `#0a0a0a` / `#ffffff26`.
///
/// Only those two forms, because only those two come out of `getComputedStyle`
/// -- this is not a CSS colour parser and should not grow into one. Anything
/// else is returned untouched: a value this does not recognise is likelier to
/// be already a hex literal from a fallback than to be worth guessing at.
function hex(css: string): string {
  const parts = css.match(/^rgba?\(([^)]+)\)$/);
  if (!parts) return css;
  const [r, g, b, a] = parts[1].split(",").map((n) => Number(n.trim()));
  if ([r, g, b].some((n) => !Number.isFinite(n))) return css;
  const byte = (n: number) => Math.round(n).toString(16).padStart(2, "0");
  // Alpha only when there is one to carry: an opaque `#rrggbbff` is the same
  // colour, but it is a longer string in every theme dump and diff for nothing.
  const alpha = a === undefined || a >= 1 ? "" : byte(a * 255);
  return `#${byte(r)}${byte(g)}${byte(b)}${alpha}`;
}
