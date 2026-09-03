// What a session is: the record, as it describes itself.
//
// Everything here is stored on the session rather than derived, which is the
// point of it -- the skills it was given, the MCP servers it was created with
// and the toolchains its image carries are all facts about *that* session, and
// they stay true after the config file that named them has changed.

import type { Session } from "../gen/Session";
import type { Usage } from "../gen/Usage";

export function Facts({ session, usage }: { session: Session; usage: Usage | null }) {
  return (
    <dl className="facts">
      <Row label="task" value={session.task || "(none)"} />
      <Row label="repo" value={session.repo} />
      <Row label="branch" value={session.work_branch} />
      <Row label="base" value={session.base_branch ?? "(the remote's default)"} />
      {/* The two rows a sandboxed session has here are the two a worktree
          session does not: it has a directory on the server instead of a
          sandbox, and no policy at all. Showing them as "(none recorded)"
          would read as a record that lost them. */}
      {session.backend === "sandbox" ? (
        <>
          <Row label="isolation" value="sandboxed" />
          <Row label="sandbox" value={session.sandbox} />
          <Row label="policy" value={session.policy ?? "(none recorded)"} />
        </>
      ) : (
        <>
          <Row label="isolation" value="none — a worktree on the server" />
          <Row label="workdir" value={session.workdir ?? "(unknown)"} />
        </>
      )}
      <Row label="agent" value={session.agent} />
      <List label="providers" values={session.providers} />
      <List label="toolchains" values={session.toolchains} />
      <List label="skills" values={session.skills.map((s) => s.name)} />
      <List label="mcp" values={session.mcp.map((m) => m.name)} />
      {/* What this session has spent, which is the half of the status line
          payload that *is* about the session -- the rate-limit windows beside
          it in the header are the account's. `null` until the agent's status
          line has run once, and a session that has spent nothing says so
          rather than showing $0.00 as though it were a measurement. */}
      {usage && (
        <>
          <Row label="cost" value={money(usage.cost_usd)} />
          {usage.model && <Row label="model" value={usage.model} />}
          {usage.duration_ms !== null && (
            <Row label="worked" value={duration(usage.duration_ms)} />
          )}
          {usage.lines_added !== null && (
            <Row label="lines" value={`+${usage.lines_added} -${usage.lines_removed ?? 0}`} />
          )}
          {/* How full the context is, which is what says whether this session
              is about to compact -- the most useful number in the payload, and
              one the plan did not ask for because nobody had looked yet. */}
          {usage.context_used_percentage !== null && (
            <Row
              label="context"
              value={`${Math.round(usage.context_used_percentage)}%${
                usage.context_size ? ` of ${Math.round(usage.context_size / 1000)}k` : ""
              }`}
            />
          )}
        </>
      )}
    </dl>
  );
}

/// Two decimals, because that is what a cost is. `null` is not zero: it means
/// the agent has not said, which for a session that has just started is the
/// truth.
function money(usd: number | null): string {
  return usd === null ? "(not reported yet)" : `$${usd.toFixed(2)}`;
}

/// Minutes and seconds of wall clock. Not a relative age like the tree's: this
/// is how long the agent has been *working*, which is a duration and not a
/// point in time.
function duration(ms: number): string {
  const secs = Math.round(ms / 1000);
  if (secs < 60) return `${secs}s`;
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins}m ${secs % 60}s`;
  return `${Math.floor(mins / 60)}h ${mins % 60}m`;
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <>
      <dt>{label}</dt>
      <dd>{value}</dd>
    </>
  );
}

/// An empty list says so rather than being omitted. "no skills" and "this pane
/// forgot to render skills" look identical when the row is missing, and the
/// first is a thing worth knowing.
function List({ label, values }: { label: string; values: string[] }) {
  return (
    <>
      <dt>{label}</dt>
      <dd>{values.length ? values.join(", ") : <span className="none">none</span>}</dd>
    </>
  );
}
