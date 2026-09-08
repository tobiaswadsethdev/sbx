// The window noticing that a newer release exists, and offering to take it.
//
// **Windows only, and that is not an oversight.** The release page carries a
// Windows installer and no Linux one, because a Tauri bundle links against the
// webkit2gtk of the distribution that built it -- see docs/install.md. On Linux
// the window is built from the tree, so there is nothing for an updater to
// fetch and this stays out of the way rather than offering something that would
// fail.
//
// **It asks.** The download is verified against a signature the release was
// built with, so the risk is not what arrives; it is *when*. A window watching
// four agents is a window somebody is using, and replacing it out from under
// them mid-session is the same mistake `sbxd` refuses to make with its own
// binary. So: a bar, a button, and it waits.
//
// The check is one request at launch and never again. A window left open for a
// week is not a thing to poll github about, and the next launch is soon enough
// for a release that has been out for hours.

import { useEffect, useState } from "react";

/// What was found, once the check has come back with something.
type Found = {
  version: string;
  /// Kept so the install uses the very object the check returned. Re-checking
  /// on click would be a second request with a second answer, and the version
  /// in the bar has to be the version that installs.
  install: () => Promise<void>;
};

type Phase =
  | { at: "idle" }
  | { at: "found"; found: Found }
  | { at: "installing"; version: string }
  | { at: "failed"; version: string; why: string };

/// Whether this build has an updater behind it at all.
///
/// `navigator.userAgent` rather than a Tauri call, because the answer decides
/// whether to *make* the call: the plugin is compiled in on Windows only, and
/// asking a no-op plugin for an update is an error to handle rather than a
/// question with an answer.
function onWindows(): boolean {
  return /windows/i.test(navigator.userAgent);
}

export function UpdateBar() {
  const [phase, setPhase] = useState<Phase>({ at: "idle" });
  const [dismissed, setDismissed] = useState(false);

  useEffect(() => {
    if (!onWindows()) return;
    let live = true;

    (async () => {
      try {
        // Imported here rather than at the top of the file so a Linux build
        // never loads the plugin's JS at all -- and so a browser opening the
        // dev server, which has no Tauri host, does not throw on import.
        const { check } = await import("@tauri-apps/plugin-updater");
        const update = await check();
        if (!live || !update) return;
        setPhase({
          at: "found",
          found: {
            version: update.version,
            install: () => update.downloadAndInstall(),
          },
        });
      } catch {
        // Silence is right here. Nothing is broken, the window works, and
        // "could not reach github" is not worth a bar across the top of it.
        // `sbxd doctor` is where a version question gets a real answer.
      }
    })();

    return () => {
      live = false;
    };
  }, []);

  if (dismissed || phase.at === "idle") return null;

  if (phase.at === "installing") {
    return (
      <div className="update">
        <span className="what">installing {phase.version}…</span>
        <span className="note">the window restarts on its own</span>
      </div>
    );
  }

  if (phase.at === "failed") {
    return (
      <div className="update">
        <span className="what">could not install {phase.version}</span>
        <span className="note">{phase.why}</span>
        <a
          href={`https://github.com/tobiaswadsethdev/sbx/releases/tag/v${phase.version}`}
          target="_blank"
          rel="noreferrer"
        >
          download it instead
        </a>
        <button onClick={() => setDismissed(true)}>dismiss</button>
      </div>
    );
  }

  const { found } = phase;
  return (
    <div className="update">
      <span className="what">sbx {found.version} is available</span>
      <button
        className="take"
        onClick={async () => {
          setPhase({ at: "installing", version: found.version });
          try {
            await found.install();
            // The installer replaced the files; the process still running is
            // the old one, so it has to go. `relaunch` is the process plugin
            // rather than `window.location.reload()`, which would reload the
            // web view and leave the same binary behind it.
            const { relaunch } = await import("@tauri-apps/plugin-process");
            await relaunch();
          } catch (e) {
            setPhase({
              at: "failed",
              version: found.version,
              why: e instanceof Error ? e.message : String(e),
            });
          }
        }}
      >
        install and restart
      </button>
      <button onClick={() => setDismissed(true)}>later</button>
    </div>
  );
}
