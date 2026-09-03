// An OS notification when a session starts waiting on you.
//
// The single largest quality-of-life gain this window has over the terminal:
// watching four agents is exactly the case where a terminal loses, and a
// `waiting` badge in a list you are not looking at is a badge nobody sees.
//
// **On the transition, not on the state.** A session sits in `waiting` until
// somebody answers it, and the list is re-read every few seconds -- so
// notifying on the state would notify every three seconds for as long as the
// agent waits. What is worth interrupting for is the moment it *starts*.
//
// And not on the first list either. Opening the window with three sessions
// already waiting should not fire three toasts about things that have been true
// for an hour; the first list is what the window learns rather than what has
// just happened.

import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";

import type { Session } from "./gen/Session";

/// The states seen last time, so a transition can be told from a state.
///
/// Module-level rather than React state on purpose: this must not re-run when a
/// component re-renders, and a `useRef` in the component that happened to
/// unmount -- switching servers does -- would forget what it had seen and
/// notify again for everything already waiting.
let seen: Map<string, string> | null = null;

/// Whether the OS has said yes. Asked once, lazily: asking on startup is a
/// permission dialog in front of a window somebody has just opened, and the
/// answer is only needed the first time an agent actually waits.
let allowed: Promise<boolean> | null = null;

function permitted(): Promise<boolean> {
  allowed ??= (async () => {
    if (await isPermissionGranted()) return true;
    return (await requestPermission()) === "granted";
  })().catch(() => false);
  return allowed;
}

/// Tell the OS about anything that has just started waiting.
///
/// Called with every session list. Returns what it notified about, which is
/// what makes it testable: the decision is `transitions`, and this is the
/// side effect.
export function onSessions(sessions: Session[]): string[] {
  const now = new Map(sessions.map((s) => [s.name, s.state as string]));
  const first = seen === null;
  const started = first ? [] : transitions(seen!, now);
  seen = now;

  if (started.length > 0) {
    void notify(started, sessions);
  }
  return started;
}

/// Which sessions have just started waiting.
///
/// A session that was not in the previous list -- one created a moment ago --
/// counts if it is already waiting: it did just start. A session that has
/// disappeared cannot be waiting for anything.
export function transitions(before: Map<string, string>, after: Map<string, string>): string[] {
  const started: string[] = [];
  for (const [name, state] of after) {
    if (state !== "waiting") continue;
    if (before.get(name) !== "waiting") started.push(name);
  }
  return started;
}

async function notify(names: string[], sessions: Session[]) {
  if (!(await permitted())) return;
  // One notification for several, because four toasts stacking up is worse
  // than one that says four. The body names them, since which one is waiting
  // is the whole question.
  const title =
    names.length === 1 ? `${names[0]} is waiting for you` : `${names.length} sessions are waiting`;
  const body = names
    .map((name) => {
      const branch = sessions.find((s) => s.name === name)?.work_branch;
      return branch ? `${name} · ${branch}` : name;
    })
    .join("\n");
  try {
    sendNotification({ title, body });
  } catch {
    // A notification that will not send is not worth a message in the window:
    // the list already shows what it would have said.
  }
}

/// Forget what has been seen. For switching servers, where the sessions are a
/// different set entirely and the previous states say nothing about them.
export function reset() {
  seen = null;
}
