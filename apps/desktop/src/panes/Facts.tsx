// What a session is: the record, as it describes itself.
//
// Everything here is stored on the session rather than derived, which is the
// point of it -- the skills it was given, the MCP servers it was created with
// and the toolchains its image carries are all facts about *that* session, and
// they stay true after the config file that named them has changed.

import type { Session } from "../gen/Session";

export function Facts({ session }: { session: Session }) {
  return (
    <dl className="facts">
      <Row label="task" value={session.task || "(none)"} />
      <Row label="repo" value={session.repo} />
      <Row label="branch" value={session.work_branch} />
      <Row label="base" value={session.base_branch ?? "(the remote's default)"} />
      <Row label="sandbox" value={session.sandbox} />
      <Row label="policy" value={session.policy ?? "(none recorded)"} />
      <Row label="agent" value={session.agent} />
      <List label="providers" values={session.providers} />
      <List label="toolchains" values={session.toolchains} />
      <List label="skills" values={session.skills.map((s) => s.name)} />
      <List label="mcp" values={session.mcp.map((m) => m.name)} />
    </dl>
  );
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
