# ai-sandboxer

A terminal UI for running several coding agents in parallel, each in its own
NVIDIA OpenShell sandbox. Claude Squad's workflow, with real isolation
underneath: kernel-enforced filesystem/network/process policy per session,
credentials injected at runtime instead of sitting on disk, and an audit trail
of every allow/deny decision.

Working binary name: `sbx` (changeable).

## Locked decisions

| Decision | Choice | Why |
| --- | --- | --- |
| Stack | Rust + ratatui | Matches OpenShell's own implementation language; single static binary; can link their crates later |
| Working copy | Clone inside the sandbox, publish a branch | Strongest isolation; host never hands the agent a live worktree |
| Attach | tmux **inside** the sandbox, attached via `exec --tty` | Revised in increment 4. The agent survives losing its connection, `capture-pane` works without anything host-side, and a layer disappears. `sandbox connect` cannot be used: it takes no remote command |
| OpenShell interface | CLI subprocess (`openshell ... --output json` where available) | Python SDK is broken in 0.0.45 and thinner than the CLI; no Rust SDK published. Isolated behind one trait so it can be swapped for gRPC later |
| Bind mounts | Opt-in only, never default | NVIDIA docs: bind mounts can negate workspace isolation and filesystem policy |

## Architecture

```
                +-------------------------+
                |        sbx (TUI)        |   ratatui + crossterm
                |  list | preview | diff  |
                +-----------+-------------+
                            |
        +-------------------+--------------------+
        |                   |                    |
   SessionStore        OpenShell client      TmuxManager
   (~/.config/sbx/     (CLI subprocess,      (host tmux sessions,
    sessions.json)      one trait)            capture-pane, attach)
                            |
                    openshell gateway (docker driver)
                            |
        +-------------------+--------------------+
        |                   |                    |
    sandbox A           sandbox B            sandbox C
   clone+agent         clone+agent          clone+agent
```

### Session lifecycle

1. **Create** — user gives a repo + a task prompt. `sbx` creates a sandbox
   (`--label sbx.session=<id>`, policy from a named template, providers for
   git + model API), waits ready.
2. **Seed** — inside the sandbox: clone the repo, `git switch -c sbx/<slug>`,
   start `tmux` + the agent with the initial prompt.
3. **Attach** — host tmux session whose pane runs `openshell sandbox connect`;
   Enter attaches fullscreen, detach returns to the TUI.
4. **Observe** — poll status (agent hook file, falling back to
   `capture-pane` scraping) and `git diff --stat` for the diff pane.
5. **Publish** — push the branch, open a PR, or export a patch to the host.
6. **Reap** — delete the sandbox; keep the branch/patch.

### Session record

```
id, name, repo_url, base_branch, work_branch, task_prompt,
sandbox_name, tmux_session, policy_template, agent,
state: Creating|Seeding|Running|Waiting|Idle|Failed|Published|Dead,
created_at, last_activity, diff_stat
```

## Increments

Each increment ends in something runnable and is committed separately.
Increments 0-6 are done; 7 onward are specified but not started.

- **0. Ground truth** — DONE except agent auth. CLI 0.0.45 -> 0.0.110,
  gateway installed from tarballs (Arch has no dpkg/rpm) and running as a
  systemd user service, mTLS-authenticated. Verified by hand: sandbox create
  (~1s warm), policy application, per-binary network enforcement, git clone,
  host-tmux attach via `sandbox connect`, capture-pane and send-keys. Written
  up in `docs/manual-loop.md`. Agent auth resolved via a subscription OAuth
  token in a custom provider profile, verified end to end. Outstanding: a real
  `git push` (needs a token + a scratch repo).
- **1. Skeleton + client** — DONE. Cargo workspace (`openshell-client`, `sbx`),
  `OpenShell` trait over the CLI with typed errors, unit tests over captured
  0.0.110 JSON, `#[ignore]`d live integration tests (create/exec/delete
  roundtrip, 1.8s), and `sbx doctor`. Clippy and rustfmt clean.
- **2. Session store** — DONE. `sbx new` / `ls` / `rm`, seeding (clone, work
  branch, host git identity), pure reconciliation against the gateway, and
  adoption of live sandboxes after total cache loss. The sandbox rather than
  the cache is the source of truth: labels cannot hold a URL or a branch, so
  the record lives in `/sandbox/.sbx/meta.json`.
- **3. TUI shell** — DONE. ratatui list + preview panes, vim keys, 3s
  background refresh, colour-coded states. All gateway I/O runs on a worker
  thread; the render thread never blocks on a subprocess. Bare `sbx` launches
  it. Verified by driving it inside tmux and capturing the rendered panes.
- **4. Attach** — DONE. Custom image (`sbx-base`, community base plus tmux,
  Dockerfile embedded in the binary), the agent started under an in-sandbox
  tmux session with the task as its opening prompt, Enter attaches from the
  TUI, `Ctrl-b d` returns. Also `sbx attach` and `sbx image build`.
- **5. Diff pane** — DONE. `Tab` cycles the right pane between preview and
  diff, remembered per session; `h`/`l` move focus and the movement keys follow
  it, so `j`/`k` walk the list on the left and scroll on the right. One exec
  fetches three sections -- committed (`diff base...HEAD`, from the merge-base,
  so commits landing on the base branch afterwards are never credited to the
  agent), uncommitted (`diff HEAD`, staged and unstaged together) and untracked,
  which appears in neither and is the most common thing an agent produces.
  Diff-only colouring, capped at 2000 lines per section with a visible notice.
  A `+12/-3 ?` column in the list, round-robined across sessions. Both panes
  refetch on a timer, so a diff stays current while the agent edits underneath
  it. Also `sbx diff <name>`. Verified against a live sandbox under tmux,
  including the live-update, truncation, no-changes and hostile-branch-name
  paths.

  Deferred, deliberately: `syntect` did not earn its weight -- add/remove/hunk
  colouring is what makes a diff readable, and a syntax set would outweigh the
  rest of the binary. No horizontal scrolling either, so a line wider than the
  pane is clipped; wrapping a diff is worse, and the fix is a `<`/`>` binding
  when it starts to hurt.

- **6. Status detection** — DONE. The state column now says what the *agent* is
  doing, not just that the sandbox is up. Two sources, combined in
  `crates/sbx/src/status.rs`: Claude Code hooks baked into the image write
  `/sandbox/.sbx/status.json` via a `sbx-status` script on PATH, and
  `tmux capture-pane` is matched against markers taken from real specimens
  committed under `crates/sbx/tests/panes/`. A waiting session is a filled
  magenta badge plus a count in the list title, so it is legible even scrolled
  out of view; the preview pane names the tool in play and which source decided.
  Shares increment 5's exec budget rather than opening a second one -- the stat
  and the status come back from one `ops::poll`. Also reported by `sbx ls`, and
  `sbx doctor` warns when the image predates the hooks, since an old image looks
  entirely healthy while silently never reporting.

  **The plan had this backwards, and only running it showed why.** The hooks
  were meant to be primary with the pane as a fallback for agents without them.
  Measured against Claude Code 2.1.143, the hooks cannot see two of the three
  states:

  * No `Notification` fires for a permission prompt. A sandbox sitting on "Do
    you want to proceed?" reports `running`/`Bash` from `PreToolUse` and stays
    there indefinitely -- so the hooks structurally cannot report the one state
    the whole tool exists to surface.
  * No `Stop` fires for an interrupt. Escape returns the agent to its input box
    without ending a turn, so the file keeps saying `running`. This one was
    caught only because the TUI showed `running` against a plainly idle screen.

  Both are the same failure: hooks report *events*, and there is no event for
  every state. So the pane decides, and the file contributes the tool name plus
  an answer for a sandbox with no agent pane at all. The two disagreeing is the
  normal case, which is why the preview pane shows which one won.

  Also learned: the idle input box draws the same `❯` glyph as an open menu
  (`❯ commit this`), so that glyph alone reports every idle session as needing
  attention. Only a cursor on a *numbered* option means a prompt. And the
  question wording varies by tool -- "make this edit to README.md?" versus
  "proceed?" -- so the menu is matched structurally rather than by text.

### 7. Policy layer

The capability claude-squad structurally cannot have, so it deserves to be
visible rather than buried in a YAML file.

- Named templates shipped with the binary: `readonly-explore` (no egress at
  all), `feature-work` (what exists today), `net-open` (npm, pypi, crates.io
  added). `sbx new --policy` should accept a template name as well as a path.
- Policy pane showing the effective rules per binary. The data is already
  available: `sandbox get --output json` returns the full effective policy,
  unlike `policy get` which returns only metadata.
- A keybinding to widen or tighten network rules mid-run. Network and inference
  sections hot-reload; filesystem and process sections are locked at creation,
  so the UI must not offer to change those.
- Live allow/deny feed. `openshell logs <sandbox>` emits OCSF lines
  (`OCSF NET:OPEN`, `SSH:OPEN ALLOWED`, `CONFIG:APPLYING`) that can be tailed
  into a pane. "The agent tried to reach pastebin.com and was denied" as a
  live event is the demo that sells the tool.
- Deprecation to fix while here: the gateway warns that `tls: terminate` is
  deprecated and TLS termination is now automatic; `tls: skip` disables it.

### 8. Publish

- `git push` the work branch, then `gh pr create`, both from inside the
  sandbox. Needs a GitHub token provider and `github_git` /`github_rest_api`
  in the policy -- both already written, neither yet exercised against a repo
  the account can write to. This is the one part of the loop never proven end
  to end.
- Alternative for repos with no remote: `git format-patch` and download the
  patch to the host, which keeps the isolation story intact.
- Mark the session `Published` (the state exists and is unused).

### Later, unscheduled

- **Warm pool** — less urgent than expected: sandbox creation is ~1s with the
  image cached, and cloning dominates. Prewarming the *clone* would help more
  than prewarming the sandbox.
- **Port forwarding** — `openshell forward` and `openshell service` for dev
  servers an agent starts.
- **Config file** — default policy, default provider, default repo, refresh
  interval. Everything is flags today.
- **Recovering a wedged sandbox** — after an abruptly killed attach, exec hangs
  forever for that sandbox. `sbx doctor <session>` could detect it (exec with a
  short timeout) and offer `sandbox stop && sandbox start` as a repair before
  falling back to recreating.
- **Multiple agents** — the `agent` field is already stored per session and
  hardcoded to `claude`. codex, opencode and copilot are all in the community
  policy, so most of the work is policy templates plus a launch command.
- **`sbx new` from the TUI** — creating a session still means dropping to the
  shell.

## Risks

- **OpenShell is v0.0.x and moves fast** (65 releases in ~3 months). Pin the
  version, keep all CLI knowledge in one module, snapshot-test the parsers.
- **Claude Code's TUI is not an API.** Status detection matches on rendered
  strings (`Esc to cancel`, `esc to interrupt`, `? for shortcuts`), which a
  redesign would break. Mitigation: the markers live in one module, the
  specimens under `crates/sbx/tests/panes/` are real captures, and the tests
  fail loudly rather than degrading quietly.
- **Sandbox boot latency** vs an instant tmux session. Mitigation: prebaked
  image with the agent CLI + toolchain, warm pool of idle sandboxes.
- **Diff review UX** is the main way this loses to claude-squad when code
  lives remote. Addressed in increment 5; the remaining gap is clipped long
  lines, since the pane does not scroll horizontally.
- **No first-class host mounts** (NVIDIA/OpenShell#500). Watch that issue; it
  would unlock a much better local-dev mode.
- **Overlap with `openshell term`** — that is a k9s-style resource browser.
  Stay out of its lane: `sbx` orchestrates tasks, not resources.

## Open decisions

### Agent authentication - RESOLVED

`claude setup-token` mints a long-lived OAuth token backed by the subscription,
carried by the custom `claude-code-oauth` provider profile and injected by the
gateway at runtime. No API-key billing, no interactive login per sandbox, and
the token never lands on the sandbox filesystem. Details and the L7-inspection
constraint are in `docs/manual-loop.md`.

Still worth building later: an `--auth` mode selecting between provider-injected
tokens and an in-sandbox login, since subscription-authed agents other than
Claude Code will not all support a setup-token equivalent.

## Picking this up again

Current state: increments 0-6 done, `main` at a clean tree, 88 tests, clippy
and rustfmt clean. `sbx doctor` should be all green; if the gateway is down,
`systemctl --user status openshell-gateway`.

The loop that works today:

```sh
sbx new --repo <url> --task "..." --policy policies/feature-work.yaml \
        --provider claude-oauth
sbx            # Enter to attach, Ctrl-b d to detach, q to quit
sbx rm <name>
```

Things a future session should know that are not obvious from the code:

* OpenShell is v0.0.x and moves fast. All CLI knowledge lives in
  `crates/openshell-client`. Constraints found the hard way -- sandbox names
  capped at 19 characters, label values 63 characters of `[A-Za-z0-9._-]`, the
  real phase vocabulary, Landlock blocking `/dev/pts`, `/dev/ptmx` crash-looping
  the supervisor, credentialed endpoints needing L7 inspection -- are all
  recorded in `docs/manual-loop.md`.
* Never kill an attach; wait for the user to detach. Killing it wedges exec for
  that sandbox permanently.
* The local cache is disposable by design. To test that, delete
  `~/.config/sbx/sessions.json` and run `sbx ls`.
* Live tests need a gateway and are behind `#[ignore]`:
  `cargo test -p openshell-client -- --ignored`.
* The TUI is testable without a human: run it under tmux and use
  `capture-pane` / `send-keys`. Every TUI claim so far was verified that way.
* **Exec on a single sandbox is serialised gateway-side, and one stuck exec
  blocks every later one for that sandbox.** Found while testing the diff
  script: a malformed script left `sh` waiting on input, and from then on every
  exec against that sandbox hung until the host-side client was killed. Trivial
  execs still returned, so it looks intermittent. This is why the diff pane
  spends its exec budget on the selected session only. It is also the concrete
  case behind "recovering a wedged sandbox" in the backlog.
* A `Display` impl that calls `f.write_str` silently ignores the formatter's
  width, so `{:<9}` does nothing and columns collide. `State` now uses `f.pad`.
  Worth remembering before adding another padded column.
* **The image build needs a real context, and heredocs are a trap.** `COPY <<EOF`
  requires BuildKit; the installed docker (29.6.0, no `buildx`) uses the legacy
  builder, which ignores the `# syntax=` directive and fails with "no source
  files were specified" -- a message that never mentions the builder. The build
  now writes its embedded files to a temp directory and passes that as the
  context, which works on both. Watch for `docker build ... | tail` hiding a
  failed build behind `tail`'s exit code.
* Agent status comes from the *screen*, not the hooks. See increment 6 above:
  Claude Code fires no hook for a permission prompt or an interrupt. If a future
  version adds one, `status::combine` is the single place to revisit.
