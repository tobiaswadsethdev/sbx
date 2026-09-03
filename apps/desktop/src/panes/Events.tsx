// The allow/deny feed: every decision the gateway made, newest first.
//
// The pane with no equivalent in an ADE built on git worktrees, and the reason
// the policy one above is worth reading -- a rule is a claim, and this is what
// actually happened.

import { useFetch } from "../useFetch";
import { api } from "../api";
import type { Event } from "../gen/Event";
import { Unisolated } from "./Policy";

export function EventsPane({ server, name }: { server: string; name: string }) {
  const { data, error, kind } = useFetch(() => api.events(server, name), [server, name]);

  // Nothing is deciding anything, so there is nothing to feed. Same words as
  // the policy pane, from the same place: the absence is one fact about the
  // session, not two.
  if (kind === "no-isolation") return <Unisolated said={error} />;
  if (error) return <p className="error">{error}</p>;
  if (!data) return <p className="loading">reading the feed…</p>;
  if (data.length === 0) return <p className="loading">no policy decisions in the recent log</p>;

  return (
    <ul className="events">
      {data.map((e, i) => (
        <Row key={i} event={e} />
      ))}
    </ul>
  );
}

/// `Verdict` is PascalCase on the wire where `State` is lowercase, which is an
/// inconsistency worth leaving alone: events are persisted as JSONL per session,
/// so a `rename_all` would make every file already on disk unreadable. The
/// generated type is what settles it -- this comparison was written against the
/// wrong casing and would have failed silently, colouring every denial as
/// neutral, if the types had been copied by hand instead.
const VERDICT = { Allowed: "allow", Denied: "DENY", Neutral: "-" } as const;

function Row({ event: e }: { event: Event }) {
  return (
    <li className={e.verdict.toLowerCase()}>
      <span className="clock">{clock(e.at)}</span>
      <span className={`verdict ${e.verdict.toLowerCase()}`}>{VERDICT[e.verdict]}</span>
      <span className="class">{e.class}</span>
      <span className="subject">{e.subject}</span>
      {e.policy && <span className="policy">[{e.policy}]</span>}
      {e.reason && <div className="reason">{e.reason}</div>}
    </li>
  );
}

/// UTC, matching the terminal's feed. A denial is compared against a gateway
/// log, and both being in the same zone is what makes that possible.
function clock(at: number): string {
  return new Date(at * 1000).toISOString().slice(11, 19);
}
