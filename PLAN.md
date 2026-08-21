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
| Attach | Host tmux session per task, pane runs `openshell sandbox connect` | Attach/detach, scrollback, resize and `capture-pane` previews for free; no PTY emulation in v0 |
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
- **3. TUI shell** — ratatui list pane + preview pane, vim keys, live refresh,
  no attach yet.
- **4. Attach** — tmux session per sandbox, Enter attaches, detach returns
  cleanly (suspend/restore the TUI's terminal state).
- **5. Diff pane** — `git diff` from inside the sandbox, syntax-highlighted,
  Tab switches preview/diff, `git diff --stat` in the list.
- **6. Status detection** — agent hook writing a status file inside the
  sandbox, polled over exec; `capture-pane` heuristic fallback. Drives the
  colored state column and a "needs input" indicator.
- **7. Policy layer** — named templates (`readonly-explore`, `feature-work`,
  `net-open`), a keybinding to hot-reload network rules mid-run, and a pane
  streaming policy allow/deny events. **This is the feature claude-squad
  cannot have.**
- **8. Publish** — push branch / open PR via `gh` inside the sandbox, or
  export a patch to the host. Then warm sandbox pool, port-forward for dev
  servers, config file, README.

## Risks

- **OpenShell is v0.0.x and moves fast** (65 releases in ~3 months). Pin the
  version, keep all CLI knowledge in one module, snapshot-test the parsers.
- **Sandbox boot latency** vs an instant tmux session. Mitigation: prebaked
  image with the agent CLI + toolchain, warm pool of idle sandboxes.
- **Diff review UX** is the main way this loses to claude-squad when code
  lives remote. Prove it in increment 5, not at the end.
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
