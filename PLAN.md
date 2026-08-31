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
                | list | preview | diff   |
                |      | policy  | events |
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
Increments 0-21 are done. What is left is the unscheduled list below.

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

- **7. Policy layer** — DONE. The capability claude-squad structurally cannot
  have, made visible rather than buried in a YAML file.

  Three templates embedded in the binary (`crates/sbx/src/policy.rs`, written
  out to a temp file when the CLI needs a path, the trick `image.rs` already
  used): `readonly-explore`, `feature-work`, `net-open`. `--policy` takes a name
  or a path, and a spec containing `/` or ending `.yaml` is always a path, so
  the checked-in files stay usable and a local file cannot shadow a template.
  `feature-work` is now the **default** where before no policy was applied at
  all -- for a tool whose point is isolation, inheriting the gateway's default
  was the wrong starting position.

  A policy pane and an events pane, so `Tab` now cycles four views
  (`Shift-Tab` goes back) and each keeps its own scroll offset. In the policy
  pane `w` widens to the package registries and `t` tightens back, guarded so a
  blind or overlapping change cannot be issued. `sbx policy`, `sbx events` and
  `sbx policies` expose the same three things to the shell. `tls: terminate` is
  gone from every template; verified that termination still happens, `engine:l7`
  still decides, and the per-create deprecation warnings stop.

  **The plan was wrong about where the data lives, and about what a policy
  change means.** Four things only running it showed:

  * `policy get --full` is the call, not `sandbox get`. Both carry a `policy`,
    but only `policy get` reports `active_version` alongside `version` -- and
    without that there is no way to tell a *submitted* policy from an *enforced*
    one, which is the difference the pane exists to show during the ~6s a
    revision takes to load.
  * **A filesystem change is accepted, reported as effective, and never
    enforced.** `policy set` with an extra `read_write` path returns "Policy
    version 4 loaded", `policy get --full` then lists the new path, and every
    subsequent Landlock application still logs the creation-time count
    (`rw:4`, not 5). So the gateway's own answer for that section cannot be
    trusted on a live sandbox. The pane renders it under a notice saying so;
    the plan's "the UI must not offer to change those" was right for a weaker
    reason than the real one.
  * **`rules:` is default-deny, but only without `access:`.** An endpoint with
    an allow-list and no access class denies every unlisted path (measured:
    `/get` 200, `/ip` 403). Add `access: read-only` next to the same allow-list
    and the unlisted path is allowed again -- the two grant the *union*. This
    was found by writing the test the wrong way round first, and it is the kind
    of thing that makes a policy read as a restriction while granting far more.
    The pane says `(rules only)` or names the class, and warns when both are set.
  * **The gateway names the rules, and makes more of them than you ask for.**
    `--rule-name` is rejected for a multi-endpoint update, and three
    `--add-endpoint` flags become *three* rules (`allow_pypi_org_443`, ...),
    one per endpoint, each with the full binary list. So "is the preset already
    applied?" is answered by matching endpoints, never names.

  Also learned, and the reason the events pane is usable at all: **the feed is
  mostly the observer.** `sbx` polls once a second, every poll opens an exec,
  and every exec logs an ssh relay open, an `SSH:OPEN ALLOWED`, a relay close
  and a `CONFIG:APPLYING`/`CONFIG:BUILT` pair as Landlock is applied to the new
  process. Five events a second, all of them ours, and a real denial scrolled
  off the top within a second. The filter keeps decisions plus anything graded
  above `INFO` -- that second clause is what preserves `CONFIG:VALIDATED [MED]`,
  the only channel the gateway has for saying a policy key is deprecated, and
  how the `tls: terminate` removal was found in the first place.

  Deferred, deliberately: the events pane does not wrap. A wrapped continuation
  starts at column zero, which puts a URL fragment where a verdict should be and
  destroys the columns the feed is scanned by; `sbx events` prints the full text.
  `--tail` is unused too -- streaming needs a thread and a way to stop it, and
  refetching a bounded window on a timer is what every other pane already does.
  `net-open` covers npm and PyPI but not crates.io as sketched above: the image
  ships no Rust toolchain, so the endpoint would be unreachable decoration.

  Untested end to end: **that `readonly-explore` denies a push.** Its policy has
  no `git-receive-pack` allow and is default-deny, and the unit tests assert
  both, but git never reaches the POST -- it bails on credentials at the
  discovery step. Proving it needs a token and a writable repo, which is
  increment 8.

- **8. Publish** — DONE, and Azure DevOps rather than GitHub first, because
  that is the forge actually in use. `sbx publish` pushes the work branch and
  opens a pull request from inside the sandbox; `P` in the TUI does the same
  behind a y/n confirmation, since a push is outward-facing and not undone by
  pressing something else. The session is marked `Published` in one place
  (`ops::publish`) so the CLI and the TUI cannot disagree. Forge is derived
  from the repo URL (`crates/sbx/src/forge.rs`), never configured.

  Verified end to end against a real private Azure DevOps repository: clone,
  commit, push, pull request created, a second publish recognising the already
  open pull request, and `readonly-explore` refusing the push at L7 -- which
  closes the gap increment 7 left open.

  **The credential model is not what the plan assumed, and this is the finding
  that mattered.** "The gateway injects the credential at runtime" is true but
  misleading. The env var the provider sets holds a *placeholder*
  (`openshell:resolve:env:v<id>_<NAME>`), and the gateway substitutes the real
  secret into an outgoing header that contains it -- including inside the base64
  of a Basic credential. Three consequences:

  * The gateway never *adds* a header. It rewrites one. A plain
    `curl https://dev.azure.com/...` sends nothing to substitute and gets a 302
    to an Entra sign-in page. Every request has to carry the placeholder
    itself, via `http.extraHeader` for git and `-H` for curl.
  * The placeholder is safe to persist. Seeding writes it into the clone's
    `http.extraHeader`, so a later push needs no special casing, and the value
    is meaningless outside that sandbox.
  * **This was never a publish-only problem.** Cloning a *private* repository
    needs the header too, so `sbx new` previously only worked against public
    repos -- for GitHub as much as Azure DevOps. Seeding is now forge-aware,
    and degrades to a plain `git` when no credential is present so a public
    repo still clones with no provider attached.

  Other things only running it showed:

  * Azure DevOps PATs are HTTP **Basic** with the token as the password
    (`base64(":" + pat)`), not bearer. Sent as a bearer token the API answers
    302 to a sign-in page rather than 401, so the failure does not look like an
    auth failure at all. `auth_style: basic` in the profile does the right
    thing, including substituting inside the base64.
  * A PAT is scoped to one organisation. The work-org token returned 401 for a
    personal org, which is a feature: one provider per organisation, attached
    per session with `--provider`, beats one broad token.
  * `dev.azure.com` serves git *and* the REST API, where GitHub splits
    github.com from api.github.com. So `_apis` is the only thing separating
    "can fetch" from "can open a pull request", which is exactly how
    `readonly-explore` is kept read-only.
  * The URL the Clone button gives you has the organisation in the userinfo
    position. Left there, git demands a password for that username *before*
    sending anything and fails with "could not read Username" while the gateway
    waits to authenticate a request git never makes. The userinfo is stripped
    for the clone and kept in the session record.
  * No `az` CLI. A pull request is one REST POST, and the Azure CLI plus its
    devops extension would put a Python runtime in the image for it; curl and
    jq are already there. `jq -n --arg` builds the body so a task string
    containing a quote cannot inject into the API call.
  * A denied push reaches git as `RPC failed; HTTP 403`, never as the proxy's
    tidier wording. An earlier matcher looked for "403 Forbidden" and let the
    denial through as an untranslated script error; it now keys off the status
    code and names both causes, since a 403 does not say which.

  Deferred, deliberately: `git format-patch` for repos with no writable remote
  is still unwritten -- the isolation argument for it stands, but nothing has
  wanted it yet. The TUI publish still uses default options; title, body, target
  and `--draft` remain CLI-only. The reason given here was that entering text in
  the TUI needed input handling that did not exist -- increment 9 built it, so
  this is now only unwritten, not blocked.

- **9. `sbx new` from the TUI** — DONE. `n` opens a repository picker over the
  host's git checkouts, then a form for everything `sbx new` takes, and the
  create runs on its own thread so the rest of the TUI keeps working while a
  sandbox is provisioned.

  The design decision worth recording: **a local repository names a remote, it
  is not a source of code.** The sandbox still clones `origin` over the gateway,
  exactly as `sbx new --repo <url>` does. `openshell sandbox upload` exists and
  would allow the alternative -- bundle the checkout, upload it, clone from the
  bundle, rewire `origin` afterwards -- and that would carry unpushed commits
  and uncommitted edits into the sandbox. It was not taken: it needs the origin
  rewiring to keep publish, base-branch resolution and the diff pane working,
  and every one of those is a new way for a session to be subtly wrong. What the
  form does instead is *say* what stays behind, counted by one `git status` and
  one `rev-list` on the picked repository. Upload remains the obvious next move
  if local-only work turns out to matter.

  How it is put together:

  * `repos.rs` is the only module that touches the host filesystem. Discovery
    walks a handful of roots (working directory, its parent, the conventional
    `~/dev`-style directories, `$HOME` at depth 1; `SBX_REPO_ROOTS` replaces the
    lot), skipping hidden and dependency directories and never descending into a
    repository it has found. Metadata is read *out of `.git`* rather than by
    running git -- `HEAD` for the branch, `config` for the origin, `commondir`
    for worktrees -- because three subprocesses per repository would turn a scan
    of a home directory into seconds. 15 repositories in 23ms on the dev box.
  * The fuzzy filter is a subsequence scorer with bonuses for consecutive runs,
    word boundaries and prefixes, so `sbx` finds `~/dev/sbx` before
    `~/work/toolbox-sbx`. No new dependency for forty lines.
  * `tui/create.rs` is a pure state machine: `Input` (a single-line field with a
    *character* cursor, so pasted non-ASCII cannot panic it), `Picker`, `Form`.
    No I/O at all -- the scan, the git inspection, the provider list and the
    create are worker requests -- so all of it is tested with synthetic key
    events, as the rest of the TUI already was.
  * `ops::create` is `cmd_new`'s body, lifted so the CLI and the TUI cannot
    drift. It reports progress through a callback: the CLI prints the steps, the
    TUI turns them into `creating` / `seeding` / `ready` on the row. It does
    **not** build the image, because `image::build` streams docker's output and
    would tear the TUI apart; the CLI calls `image::ensure` first and the create
    thread refuses with "run `sbx image build`".
  * Creating runs on a thread of its own, unlike every other worker request.
    Requests are served one at a time, so a create served inline would freeze
    the state column and every pane for the half-minute it takes. It is detached
    rather than joined on shutdown -- holding the terminal for the rest of a
    clone would be worse -- so quitting mid-create asks first.
  * The list shows the session before it exists. `App::pending` holds a row that
    is merged into the list on every refresh until the store has it, which also
    covers the race where the worker's refresh reads `sessions.json` before the
    create thread writes it. That row is deliberately not polled until it has a
    sandbox: every exec would fail, at a subprocess each.
  * Providers are preselected by *type*, not name: the agent's credential and
    the repository host's, and only when exactly one provider of that type
    exists. Two Azure PATs means no way to know which organisation was meant,
    and a wrong credential fails three steps later.

  Verified: the CLI path through the refactored `ops::create` (create, clone,
  `ls`, `diff`, `rm`) and the worker's create seam against a live gateway,
  through `Request::Create` to `Update::Created`, both against
  `octocat/Hello-World` under `readonly-explore` with no agent started.

- **10. Destroying a session, and the agent's own version** — DONE. Two things
  the tool could not do for itself: end a session without dropping to a shell,
  and run a current Claude Code.

  **`D` destroys the selected session.** `ops::destroy` is the one description of
  what that means -- delete the sandbox, drop the record -- and `sbx rm` was
  rewritten on top of it, so the CLI and the TUI cannot disagree about what is
  left behind. Three decisions worth recording:

  * **It always asks, and the question says what is at stake.** A sandbox holds
    the only copy of whatever the agent has not published, so the question
    carries the diff stat the list is already showing: `+12/-3 ? goes with the
    sandbox`. An *unpolled* session says `the sandbox and everything in it goes`
    instead -- absence of a stat is absence of knowledge, and `no changes to
    lose` would be a claim the poll never made. Capital `D`, next to `P`, for the
    same reason: the two irreversible keys are the two that are hard to hit by
    accident, and lowercase `d` is bound to nothing.
  * **The row goes when the gateway answers, not on the next refresh.** Deletion
    is asynchronous and the sandbox stays listed as `Deleting`, which
    `store::reconcile` reads as dead -- so a refresh landing in that window would
    put the row back as `dead` and make the destroy look half-done. `App::forget`
    drops the row and everything keyed by it, and clamps the cursor onto the
    neighbour rather than jumping to the top.
  * **A session still being created is refused.** The create thread writes its
    record after it finishes, so destroying mid-create would leave a record for a
    sandbox that no longer exists and a clone still running against it. Waiting
    is tens of seconds; the alternative is a mess. A sandbox that is *already*
    gone, on the other hand, is the desired end state -- the record is dropped
    and the row cleared, which is the only way to get rid of a session left
    behind by a create that died before provisioning anything.

  **The image installs the newest Claude Code, not the base image's.** The
  community base shipped 2.1.143 while 2.1.246 was current, and the agent cannot
  fix that from inside: no policy template reaches the download service, and
  /usr/local/bin is root-owned. So the version became the image's business --
  `ARG CLAUDE_VERSION=latest`, resolved against the release manifest, checksummed
  with SHA-256, verified with `claude --version` after installing, and pinnable
  with `--build-arg`. The release binary rather than `install.sh`, which lands a
  launcher under `$HOME` -- the sandbox user's *writable* home, where the agent
  could replace its own binary.

  **The trap: `latest` inside a Dockerfile is not latest.** Docker answers a
  rebuild from the cached layer, so a `latest` resolved inside the build means
  "whatever was newest the first time that layer was built" -- the exact
  staleness the step exists to fix. `sbx image build` therefore resolves the
  version on the host and passes it in, because changing the ARG is what
  invalidates the layer. `sbx doctor` compares the built image against the newest
  release and says when it has fallen behind, so this cannot rot silently again.

  **And the upgrade broke idle detection, which is why it was worth verifying by
  hand.** Increment 6's markers were taken from 2.1.143. By 2.1.246 the footer
  under the input box is no longer a fixed hint but a list -- permission mode,
  then a rotating tip, then whatever applies -- truncated to the pane width with
  an ellipsis. `? for shortcuts` now shares that slot with several other tips, so
  an idle agent frequently showed *no* idle marker and the state fell back to the
  hook file, which is stuck on `running` after an interrupt -- the bug increment 6
  existed to fix. The input box is now recognised by its *shape* (two rules with
  the prompt between them), checked after the waiting and running markers so a
  working agent is never read as idle for having one. `esc to interrupt` survived
  as an entry in the same list, and at tmux's default 80 columns that list already
  ends in `← for a…`; the image now sets `default-size 200x50`, because the width
  of an unattached pane is the width every status scrape reads. Specimens for both
  new screens are committed next to the old ones.

  Also: `DISABLE_AUTOUPDATER=1`, in the image *and* in the baked
  `settings.json`, because the gateway does not pass the image environment
  through to an exec. Without it the agent's screen carried
  `✘ Auto-update failed · Run claude doctor` for the rest of the session, and the
  events pane a denied egress with nothing behind it worth investigating.

  Verified against a live gateway: a session created on the rebuilt image
  (claude 2.1.246, 200x50 pane, no update noise), its waiting state read off a
  real permission prompt, its idle state read off the *screen* rather than the
  hook file, then destroyed from the TUI under tmux -- cancelled with `n` first,
  confirmed with `y`, sandbox gone from `openshell sandbox list` and record gone
  from `sessions.json`, with no `dead` row appearing in between.

- **11. The agent's terminal, in the pane** — DONE. `Enter` opens the selected
  agent's terminal in the right-hand pane and gives it the keyboard; `F12` gives
  it back and the terminal keeps running. `a` still hands the whole terminal
  over, unchanged.

  **The measurement that decided it was worth building.** An embedded terminal is
  a held `exec --tty` per open session, and the note above says exec on one
  sandbox is serialised gateway-side -- if that applied here, the state column
  and the diff pane would freeze for whichever session you were looking at, which
  would make the feature actively worse than attaching. Tested before writing any
  of it: with an attach held open, ordinary execs against the same sandbox
  returned in ~200ms, and the row for a session being typed at went to `waiting`
  and its stat column to `+1/-1` while its terminal was on screen. Also tested,
  and contrary to what increment 0 recorded: killing an attach did **not** wedge
  exec for that sandbox on 0.0.110.

  How it is put together:

  * `tui/term.rs` owns one `Terminal` per open session: a `portable-pty` child
    running the same attach script as `sbx attach`, a thread doing nothing but
    feeding a `vt100::Parser`, and `tui-term` drawing that parser's screen into
    the pane. Three dependencies, which for a terminal emulator is the right
    call -- this is exactly the code not to write by hand.
  * Spawned lazily. Cycling `Tab` onto the agent view shows an invitation and
    costs nothing; only asking for the keyboard spends a process. Walking a list
    of ten sessions must not leave ten attaches running.
  * Key routing is a third `Focus`. While `Focus::Agent` holds the keyboard,
    every key goes through `term::encode_key` to the pty -- including `q`, `D`
    and the answer to a pending y/n question, which outranks even the confirm
    prompt: `y` typed at an agent must never publish something behind it. The
    encoding is a pure function, so "does Ctrl-C reach the agent", "is Enter a
    carriage return and not a newline" and "is Backspace DEL" are unit tests
    rather than things to try by hand against a live sandbox.
  * `F12` is the way out, and the reason is layouts as much as collisions:
    `Ctrl-b` is the agent's own tmux prefix, `Ctrl-c` and `Esc` belong to the
    agent, and `Ctrl-]` is AltGr gymnastics on a Swedish keyboard. It is shown in
    the pane title, because an escape hatch that has to be looked up is a trap.
  * `F12` lands on the *list*, not on the pane the terminal was drawn in. Found
    by using it: leaving an agent is almost always the first half of going to
    look at another one, and landing on the pane made `j` a scroll and put an `h`
    between every pair of sessions.
  * The pty is resized to the inner pane area on every draw, so the agent's tmux
    redraws at the size it is actually being shown at.
  * The session's facts moved out of the preview's header and into a pane under
    the list, because a terminal in the right-hand pane hid them -- and they are
    exactly what you want beside a terminal. Each fact is one row cut to the
    pane, not wrapped: the first attempt wrapped, and a repository URL is one
    unbreakable word, so the pane was taller than the character count that sized
    it and the `agent at` line fell off the bottom. The preview keeps the task
    and the repository in full.
  * `PageUp` and `PageDown` are forwarded like everything else, which is what
    makes a long session scrollable; see below for why that is the whole
    implementation.

  **Two bugs found by watching the sandbox rather than the screen.** tmux resizes
  a window to its latest client and keeps that size after the client leaves, and
  the status scraper reads that window -- so closing an embedded terminal used to
  leave the agent's window pinned to whatever the pane happened to be, one narrow
  pane away from truncating the markers increment 10 had just widened the window
  for. And the clean detach on the way out (`Ctrl-b d`, so no client is left
  listed as attached) lost a race with the kill that followed it. `Terminal::drop`
  now resizes back to `SCRAPE_SIZE`, waits for the detach to land, and only then
  kills; verified by checking `list-clients` and `#{window_width}` inside the
  sandbox after quitting, which is the only place the difference is visible.
  `SCRAPE_SIZE` and the image's `default-size` are the same number, with a test
  that says so.

  **Scrolling belongs to the agent, and both obvious implementations were
  wrong.** A parser-side scrollback collects nothing, because what arrives over
  the pty is a full-screen client repainting itself rather than lines scrolling
  off. Reaching into the sandbox tmux's 50k-line history collects nothing either:
  Claude Code runs on the *alternate* screen -- `#{alternate_on}` is 1 and
  `#{history_size}` is 0 -- which is precisely the mode that means "no scrollback
  here". It keeps its own transcript and scrolls it on page-up. The first
  implementation intercepted `PageUp` to enter tmux copy mode and so replaced a
  working scrollback with an empty one; it looked like it worked once, because a
  single page of the *previous* screen was still there. Sending the key straight
  through scrolls properly and progressively -- verified by paging back to the
  banner at the top of a session and down again. The whole feature is now the
  absence of a special case, which is the sort of thing only a live test tells
  you.

  Deferred, deliberately: the mouse and paste. Mouse capture is terminal-wide
  state, so it would take text selection away from the entire interface and can
  be left enabled by a panic; paste has the same shape. Neither is what the pane
  is for.

  Verified against a live gateway, driving the TUI under tmux: opened an agent,
  typed a prompt into it and got an answer, answered a permission prompt with
  `1`, watched the list say `waiting` and then `+1/-1` for that same session,
  left with `F12`, walked to another session and back to find the terminal still
  live with its history, destroyed the session with its terminal open, and quit
  with one open -- checking afterwards that no client was left attached, the
  window was back to 200x50, and no `openshell` child had leaked.

- **12. A face closer to Claude Squad's** — DONE. The reference is
  [claude-squad's screenshot](https://github.com/smtg-ai/claude-squad/blob/main/assets/screenshot.png);
  what was taken from it is shape rather than colour.

  * **A session is two rows and a gap**, numbered, with the branch on a dimmed
    second line and the diff stat and age right-aligned under the state. One line
    had to answer two different questions -- which session is this, and where has
    it got to -- and paid for it by truncating the name to fifteen columns with
    nowhere to put the branch. The numbers are live: `1`-`9` select that session
    from either pane, which is faster than walking `j` to it.
  * **The right pane's views are tabs along its top border**, active one in the
    accent. On the border rather than in a row of their own because the pane
    keeps its full height that way, which matters most for the view that is a
    live terminal. The session name and the scroll position moved to the other
    end of the same border.
  * **The footer is keys, not a sentence**: `(key, what it does)` pairs, key in
    the accent and the word after it grey, `·` between keys and `│` between
    groups of them, grouped by moving / acting / leaving. It is data now, so a
    new binding is a tuple rather than a string to re-punctuate.
  * **The accent is `LightBlue`, and ANSI on purpose.** Claude Squad's violet is
    a hard-coded RGB, which fights a light terminal theme; the shapes are what
    make this recognisable, not the hue. Not magenta, because magenta belongs to
    the `waiting` badge and has to stay the only thing on screen wearing it.
  * The selection is a quiet fill plus a bar in the accent, rather than reversed
    video, which turns every coloured span inside out -- a state word is least
    readable exactly when it is selected.

  **The bug that needed a colour assertion to find.** A list's `highlight_style`
  is patched *over* the row, so the selection's fill replaced the `waiting`
  badge's magenta background and left its black text on dark grey: the one signal
  the whole tool exists to deliver became invisible at the moment you selected
  it. On a selected row the badge is now bright magenta text instead of a fill.
  The test reads the styles back out of a rendered `TestBackend` buffer, because
  text is exactly what this class of bug does not show -- and the first version
  of that test looked fine while asserting on the wrong cells, since `str::find`
  returns a byte offset and a border row is mostly multi-byte box drawing.

- **13. The agent's screen, not its terminal** — DONE, and a deliberate reversal
  of half of increment 11. Entering a session hands the whole terminal over
  again; the agent tab is now a read-only view of its screen.

  **Why the pty went.** It worked -- typing, scrolling, permission prompts, all
  verified -- but a full-screen attach is simply better at the thing it was for:
  full width, the agent's own scrolling and mouse support, and no key routing
  standing between the user and the agent. Everything the embedded terminal added
  was compensation for being small and for owning the keyboard. What is worth
  keeping is *seeing* the agent without leaving, and that never needed a pty.
  Claude Squad turns out to have this exactly right: its Preview tab is read-only
  and attaching is a separate act.

  So the agent tab draws the capture the status poll already takes -- the same one
  that decides the state column -- which makes watching an agent free rather than
  a held `exec --tty` per session. `portable-pty`, `vt100` and `tui-term` are
  gone, and with them `Focus::Agent`, the key encoding, the escape hatch and the
  pty lifecycle. The pty implementation is in the history at the commit before
  this one, if it is ever wanted back.

  Three things the view needed that the terminal did not:

  * **The whole screen, not the last forty lines.** `PANE_LINES` was sized for
    marker detection, where every marker sits at the bottom; forty lines of a
    fifty-row pane cut off the banner and the first exchange. Now 120, still
    bounded so a pane left tall by an attach cannot flood a poll.
  * **The padding squeezed out.** The sandbox pane is 200x50 and the TUI's is
    whatever is left of the terminal. Claude Code draws output at the top and its
    input box at the bottom, so thirty blank rows sit between them and a short
    pane showed all output and no prompt. Runs of blanks collapse to one.
  * **A faster poll while it is on screen.** The view is only as live as the
    capture behind it, so the selected session's poll uses the diff pane's
    interval when the agent tab is showing -- content under the user's eyes has to
    keep up -- with the existing floor still capping the exec rate.

  **And the attach now puts the window back.** `Terminal::drop` used to restore
  the agent's tmux window to 200x50 on the way out; without it, attaching from an
  80-column terminal leaves the window 80 columns wide for the rest of the
  session, narrow enough to truncate the footer the running marker lives in. The
  attach script -- one definition now, shared by `sbx attach` and the TUI, which
  had drifted into two copies -- resizes the window and restores
  `window-size latest` afterwards, so the next client still resizes it. Verified
  by attaching from a 120x32 terminal and reading `#{window_width}` back out of
  the sandbox: 120x32 attached, 200x50 after detaching, no client left behind.

  Also fixed, found by using it: `sbx rm` followed by `sbx ls` printed
  "could not adopt sbx-x: sandbox not found". Deletion is asynchronous, so the
  sandbox is still listed while its record is already gone -- which looks exactly
  like an orphan worth adopting. `store::reconcile` now skips anything in
  `Deleting`.

- **14. Legible agents, and no boxes** — DONE. Two bugs, a simplification and a
  flatter interface, all from looking at a screenshot of the real thing.

  **Everything the agent drew came out as underscores.** Claude Code's banner,
  its box rules and its `⏸` and `❯` glyphs, in a real attach. The cause is three
  facts stacked: the community base image sets no locale at all, the gateway does
  not pass the image's environment through to an exec, and a tmux client that
  does not believe it is on a UTF-8 terminal draws with the DEC line-drawing set
  and replaces what it cannot map with `_`. So the locale has to be said three
  times, and each one covers a different process: `ENV` in the image for anything
  started from it, `set-environment -g` in the tmux conf for the agent itself
  (whose environment comes from the tmux server), and an explicit export in
  `ops::attach_script` for the *client*, which is the one the gateway strips.
  Every tmux invocation also passes `-u`, which asserts UTF-8 without depending
  on a locale at all. The colour half is `default-terminal tmux-256color` plus
  `terminal-features ",*:RGB"`, and `COLORTERM=truecolor` so the agent knows it
  may use 24-bit colour: inside tmux the pane's TERM decides what the agent
  thinks it has, and RGB has to be declared before tmux will pass a direct colour
  out to the terminal rather than flattening it.

  **The agent view now shows the colour too.** `capture-pane -pe` keeps the
  escape sequences, and `crate::ansi` -- about 150 lines, no dependency -- turns
  each line into styled spans. It is deliberately not a terminal emulator: no
  cursor, no scroll regions, no charset switching, because the capture is already
  a laid-out screen and `m` is the only sequence in it that carries meaning.
  Everything else is skipped rather than guessed at.

  The same tokenizer produces `strip`, which is what the marker search reads --
  `esc to interrupt` is not findable in a string where tmux has coloured `esc`
  separately, so status detection would have quietly stopped working the moment
  the capture gained colour. Blankness is judged on the stripped copy for the same
  reason: tmux colours the empty right-hand end of every row, so thirty "blank"
  rows are thirty rows of invisible escapes, and `squeeze` would have found
  nothing to squeeze. Two bugs in the parser came out of its own tests: the colon
  form of direct colour carries an empty colour-space field (`38:2::255:128:0`),
  which read as the red channel loses the colour; and `0m` resets to *nothing
  set* rather than to an explicit default, which is what lets the pane's own
  colours show through.

  **The preview tab is gone, and the agent's screen is the default view.** The
  preview had become the facts pane -- session, repo, branch, policy, providers --
  plus a `git status` the diff tab and the stat column already answer. It cost an
  exec per selected session for that. Dropping it removes `ops::repo_preview`,
  the `previews` map and a `Request`/`Update` pair, and it pays for the agent
  view's faster poll: the same budget, spent on the pane that answers the question
  the list raises. Four tabs now, agent first, and `RightView::default()` with it.

  **And the boxes are gone.** Four nested borders, drawn around content that is
  mostly rules already -- a diff, an agent's own input box, the policy pane's
  sections. What a border was actually carrying was which pane the movement keys
  belong to, and a heading carries that better: bold when focused, and no row
  spent on it. The create flow's picker and form keep theirs, because a modal is
  drawn over whatever was underneath it and its edge is the only thing saying
  where it stops.

  The selection went with them: a light filled block, as Claude Squad has it,
  because the dark fill that replaced the old reversed row turned out to be too
  close to the background to find. That makes every colour on that row a colour
  chosen against *white*, and the trap is what is not styled at all -- unstyled
  text keeps the terminal's default foreground, which in a dark theme is
  near-white and therefore invisible on it. So the row's own spans carry explicit
  dark colours when selected, `waiting` keeps its magenta (legible on white in
  either kind of theme, where the filled badge could not survive inside another
  fill), and the stat and the state dot keep theirs. The marker went: a solid
  block needs no arrow, and dropping it gives every row two columns back.

  With the boxes gone the layout needed its own air, so there is a margin at the
  terminal's edges, a gap between the columns, a blank row between the list and
  the facts, and a blank row under every heading. That last one is only safe
  because the title/padding arithmetic below is now understood: it is a *second*
  row before content, and `PANE_TOP` is what every caller measures with. The
  footer sheds its descriptions when the window is too narrow for them, since a
  hint line clipped mid-word reads as broken; an empty list offers only `n` and
  `q`, rather than keys that act on a selection that does not exist.

  The arithmetic behind that is worth knowing: ratatui charges a block a title
  row *and* any top padding, so asking for both left a blank line and one row
  less of content than every caller had measured for -- which cost the facts pane
  its `agent at` line. `PANE_TOP`/`PANE_SIDES` and a test over `Block::inner`
  pin it down now.

  Verified against a live gateway: a session created on the rebuilt image, the
  attach rendering `▐▛███▛█`, `❯`, `⏸` and `──────` correctly where the screenshot
  had underscores, the agent tab showing Claude Code's own colours through
  `crate::ansi`, the state column still reading `running` off the screen -- which
  is the thing the strip had to keep working -- and every tab plus the create
  modal read back from a live TUI without a box in sight.

- **15. The create form stops asking what it can work out** — DONE. Two pieces of
  friction, both found by using it on a real repository.

  **A second session in the same repository had to be renamed by hand.** With no
  task typed the name is derived from the repository, so the second one always
  collided and the form refused it until the name was edited. That makes the
  common case -- try something, try something else -- the one that needs work.
  `session::unique_name` appends a counter instead, shortening the stem to keep
  inside the gateway's name budget rather than dropping the suffix; `fix-the-readme`
  becomes `fix-the-readm-2`, which still reads as a variant of the same thing.
  Unlike `slugify` it will cut mid-word, because `fix-the-2` would not. The
  submit-time guard stays, for a name typed by hand: editing the name pins it, and
  a pinned name is the user's to be wrong about.

  **The Azure PAT was not ticked for an Azure repository.** Increment 9 preselects
  by *type* and only when exactly one provider of that type exists, on the grounds
  that two Azure PATs are two organisations and the wrong one fails three steps
  later. True, but it left the common case -- one PAT per org, the same one every
  time -- needing a correction on every create. So the store now breaks the tie:
  the providers the most recent session for the same **host and organisation** was
  given. Host and organisation rather than the exact URL, because an Azure PAT
  covers every repository in an org, which is what makes the answer useful for a
  repository never opened before. It is evidence rather than a guess -- it can only
  be wrong where the user was already wrong -- and with nothing to go on the old
  behaviour stands.

  History only breaks ties between the types a session actually wants: an Azure
  PAT used before is not ticked for a GitHub repository.

  Verified against the live store: opening the form on `Inet.Server`, which
  already had a session, derived `inet-server-2` and ticked `claude-oauth` and
  `azure-pat` -- leaving `azure-pat-personal`, the other PAT of the same type,
  alone.

- **16. Defaults the agent arrives with** — DONE. A fresh sandbox has a fresh
  `HOME`, so an agent with nothing baked for it starts on someone else's
  defaults: Sonnet on the subscription credential, manual permissions, and a
  handful of network calls the policy denies. The image's `settings.json` now
  carries four things -- a model, a permission mode, the traffic switches, and the
  status hooks it already had.

  * **`model: opus[1m]`**, an *alias* rather than `claude-opus-5[1m]`. Both give
    "Opus 5 (1M context)", measured, but the alias follows the newest Opus and
    keeps the million-token context -- which is the same trap as increment 10's
    Claude Code version, and the same answer. The banner says
    "(from .claude/settings.json)", which is how it was verified.
  * **`permissions.defaultMode: auto`**. Not `acceptEdits`, which still stops for
    everything that is not an edit, and not `bypassPermissions`, which is a
    different thing again -- `auto` is its own value in the mode enum
    (`default`, `auto`, `acceptEdits`, `plan`, `bypassPermissions`) and it judges
    each tool call rather than waving them all through. Claude Code's own words
    for it are "recommended to only use in isolated environments", which is
    exactly what this project builds. An agent that stops on its first edit is an
    agent still being babysat, which defeats the point of running several.
  * **`DISABLE_AUTOUPDATER`, `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` and
    `CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL`**. Each one is a call
    the policy denies -- telemetry to Datadog, the updater reaching
    `downloads.claude.ai`, the plugin marketplace reaching
    `raw.githubusercontent.com` -- and a denial with nothing behind it is noise in
    the pane that exists to show the denials that matter. Found by reading that
    pane rather than by guessing: the first two switches cleared the telemetry and
    left two hosts still being refused, and the umbrella variable cleared the
    rest.

  A consequence worth stating: with auto mode on, the `waiting` state fires far
  less often, because most of what used to raise a prompt no longer does. The
  detection is unchanged and still catches a genuine question; there is simply
  less to catch, which is the point.

  Verified against a live gateway: a session created on the rebuilt image reported
  `Opus 5 (1M context)`, announced auto mode, read and edited the repository
  without stopping to ask, showed `+1/-1` in the list, and produced an events feed
  with zero denials in it.

- **17. The intervals, measured instead of guessed** — DONE. The interface felt
  slow, and the cause was not the transport: it was that every interval had been
  chosen against a cost model an order of magnitude out.

  Measured against a live gateway: `sandbox list` 20ms, an exec that does nothing
  44ms, a full poll -- diff stat, agent state and the agent's screen -- 56ms,
  `openshell logs` 14ms for 400 lines. Even `git status --porcelain` on a ten
  thousand file, 238MB repository is 65ms. The original numbers (a 3s refresh, a
  6s poll, a one *second* floor between polls) were written when an exec was
  believed to cost hundreds of milliseconds, which is what it costs under load or
  with a client attached, not at rest.

  So: the refresh is 1s, a background session polls every 2s, the selected one
  every 500ms, the diff and events panes are 1.5s, and the floor between polls is
  200ms -- five a second across all sessions, which keeps ten of them inside their
  own interval. End to end, from touching a file in a sandbox to seeing it in the
  list: **298ms, 536ms, 523ms** over three trials, against ~850ms at the first
  attempt and six seconds before that. The TUI process sits at 0.2% CPU doing it.

  Two consequences handled rather than discovered later:

  * Every exec writes three events to the gateway's log, and the events pane reads
    a window on the end of that log. Polling five times harder shrinks how much
    *time* that window covers, so `LOG_LINES` went from 400 to 1500 -- the filter
    already drops sbx's own noise, and the read is 14ms.
  * A test asserts the budget is coherent, because the parts have to add up: the
    selected session sooner than the rest, the floor below the interval it is
    bounding, a full round inside `POLL_TTL` for a list of ten, and the redraw
    quicker than anything it draws.

  **Streaming was considered and is not needed.** One long-lived exec per session
  emitting on a loop would make this properly live and cost one round trip per
  session rather than one per read -- and increment 11 proved a held exec does not
  starve the others. But at 56ms a poll, the polling model already lands inside
  half a second, and the machinery a stream needs (a supervisor per session, a
  protocol to parse, backpressure) buys tenths of a second. The note stays here in
  case the numbers ever change: what to split first is the *stat* from the rest,
  since state and screen are a file read and a `capture-pane` and only the stat
  needs git.

- **18. The cache had a race, and the list had too little air** — DONE. Three
  small requests and one real bug they led to.

  The list: a blank row either side of each session's two rows, so a list of
  four reads as four things rather than a paragraph -- and so the selected one's
  light block has room around its text. The state dot is gone; the coloured word
  beside it was already saying the same thing twice. And nothing says
  "refreshing" any more: at a one second interval that label was on screen more
  often than off it, and a flicker carrying no information is worse than no label.

  **The bug, found by looking at why a session said `seeding` when its sandbox was
  perfectly healthy.** `sessions.json` has more than one writer and that is the
  normal case, not an edge: a TUI reconciles the whole list on a timer while a
  `sbx new` in another terminal walks a session through `creating`, `seeding`,
  `ready`. Both did load-modify-save with no lock, so the second write won and the
  first was lost -- and with increment 17 taking the refresh from three seconds to
  one, the window went from occasional to reliable. Worse, `ops::create` held a
  snapshot from *before* its clone, which on a 238MB repository is minutes old;
  `save`'s own doc comment claimed it reloaded per step, and it never had.

  Every change now goes through `store::update`, which takes an exclusive lock on
  a file beside the cache, reads, applies and writes inside it. Gateway calls and
  execs stay outside, so the lock is held for a file read and a rename. `refresh`
  merges rather than replacing wholesale, because it only knows what it loaded and
  a create in another process may have added a record since. Verified live: four
  creates from a second terminal against a TUI refreshing every second, all four
  `ready`; before the fix, one of two was `seeding`.

  **And a repair, for the records already wrong.** A record stuck in
  `creating`/`seeding` is either an abandoned create -- the create thread is
  detached, so quitting the TUI mid-clone is enough -- or the loser of the race
  above. The first refresh of any command now re-reads such a session's metadata
  and takes the sandbox's word for it: `space-b: record said seeding, sandbox says
  ready`. A session whose sandbox has *no* metadata is left alone, because that is
  exactly what a clone still running looks like -- which is what tells an
  abandoned create apart from a slow one, without a timeout to guess at.

- **19. Seeding survives the tool that asks for it** — DONE, and it closes the
  failure increment 18 could only report: a create that died left a sandbox
  holding 69MB of a 238MB clone, no `HEAD`, and a record that said `seeding`
  forever.

  The clone used to be a child of the host process, over one long `exec`. Now the
  host writes a script into the sandbox, starts it with `setsid`, and watches a
  state file. The script does everything -- clone, identity, work branch, the
  metadata record, and the agent's tmux session -- so a session comes up complete
  with nobody watching. `ops::create` is a *watcher*: it reports each step, and if
  it runs out of patience (fifteen minutes) it says the session is still seeding
  and leaves it to finish. Quitting the TUI now costs the progress report, not the
  session.

  **The state file is the point.** Three things used to look identical from
  outside a sandbox -- still cloning, finished while nobody was looking, and
  stopped -- and telling them apart is what makes an honest record possible. The
  seeder announces each step before doing it, writes its own pid, and traps any
  failure into `failed <the last lines of its log>`. The repair pass reads that
  instead of guessing: `done` is `ready`, `failed` carries git's own words, a step
  with a live pid is left alone however long it takes, and a step with a dead pid
  is a sandbox that went out from under its seeder.

  **Two things only a live run could have found.** `/bin/sh` in the sandbox is
  dash, which has no `$LINENO` -- and reaching for it *inside* the failure handler
  under `set -u` made the handler fail, so a clone that could not authenticate
  wrote no reason at all and the host had to infer "stopped" from a missing
  process. And `tr '\\n'` in a Rust raw string is a backslash and an `n`, not a
  newline, so the first working error message came back with every `n` replaced by
  a space: `could ot read User ame`.

  Verified against a live gateway, all three paths: a normal create (all four
  steps, `done`, agent running); `SIGKILL` to the host two seconds into a clone,
  after which the sandbox finished on its own and `sbx ls` reported
  `seed-kill: seeding -> ready (seeding finished)`; and a clone that cannot
  authenticate, which now says
  `seeding failed: fatal: could not read Username for 'https://github.com'` and
  leaves the record `failed` rather than `seeding`.

- **20. A diff you can scroll, and a feed that is a record** — DONE. Two reports,
  one a discoverability failure and one a real design flaw.

  **"I cannot scroll in the diff."** It scrolled -- after `l` to focus the pane,
  which is what the footer said and not what anyone tries. `Shift-↑`/`Shift-↓` now
  scroll the right-hand pane from either side, as Claude Squad does it, and
  `PageUp`/`PageDown` page it: paging a list of half a dozen sessions was never
  worth a binding, and paging the content always is. The footer lost `a attach`
  and `pgup/pgdn page` in exchange -- `enter` already does the first and
  `shift-↑/↓` implies the second -- because the described hints had grown past what
  a 120 column terminal can show, which is its own kind of unhelpful.

  **"The events log gets cleared when closing the app and opening it again."** It
  was, and by us. The pane asks the gateway for the last 1500 log lines, and every
  exec sbx makes to read a sandbox writes three lines of its own; at increment
  17's intervals a measurement said it plainly -- that window covered **125
  seconds** and contained **one** event worth showing, out of 1500 lines of which
  376 were sbx's own execs. Nothing was cleared; everything older than two minutes
  had simply rolled out, and reopening the tool made it obvious.

  So the feed is ours to keep: one JSONL file per session beside the session
  cache, each fetch merged into it, deduplicated on (timestamp, class, subject),
  trimmed to the newest 4000, written through a temp file and a rename like the
  cache. A fetch that returns nothing -- an unreachable gateway, a quiet window --
  leaves the history alone rather than emptying it. Destroying a session forgets
  its file, since it is about a sandbox that no longer exists.

  Proved end to end rather than argued: 14 events recorded from a session's clone
  and first turn, then 550 execs fired at the same sandbox until the gateway's
  window no longer contained a single one of them -- `grep -c` on the oldest
  timestamp went to zero -- after which `sbx events` still listed all 14, clone
  included.

  Worth knowing for anything else that reads that log: raising `LOG_LINES` (which
  increment 17 did, 400 to 1500) buys time linearly and loses to the poll rate
  immediately. Keeping what has been seen is the only thing that scales.

- **21. A config file** — DONE. `~/.config/sbx/config.toml`, beside the session
  cache and the events history, holding the seven things that were flags on every
  command: `gateway`, `repo`, `base`, `policy`, `providers`, `repo_roots` and
  `refresh`. Everything optional, everything a *default* -- a flag wins, and so
  does an explicit choice in the create form. `sbx config` shows what is in force
  and marks each line `*` (from the file) or `-` (built in); `sbx config --init`
  writes a commented starter file with every key commented out, so creating it
  changes nothing.

  **A file that cannot be read stops the command.** Unknown keys are rejected by
  name (`unknown field 'polciy', expected one of ...`), a `policy` that is neither
  a template nor a path is rejected against the template list, an empty
  `providers = []` is rejected rather than read as "no credentials", and a blank
  string is unset rather than a value. Every command refuses to run until it is
  fixed -- except `sbx doctor`, which is the command you reach for when something
  is wrong, so it reports the error as a failed check and carries on with the
  built-in defaults. Silently ignoring a config someone wrote is the same failure
  as the gateway reporting a policy it is not enforcing.

  **`refresh` is one number, not six.** The five measured intervals from
  increment 17 are related to each other -- the selected session polls faster
  than the rest, the floor keeps a long list from becoming a stream of execs, a
  diff has to keep up with the agent editing under it -- so `Intervals::scaled`
  multiplies all of them by `refresh / 1s` and the relationships hold by
  construction. `the_poll_budget_is_coherent` now runs over every value the
  parser accepts rather than only the tuned set. The floor is 250ms because
  `TICK` is the one interval that does *not* scale: below that the 100ms input
  poll becomes the limit and the extra `git status` inside every sandbox buys
  nothing. Measured against a live session: 41 execs in a 30s window at the
  default, 13 at `refresh = "4s"`.

  **Where a default replaces a heuristic, and where it does not.** `providers`
  replaces the create form's guesswork outright, because an explicit list is a
  better answer than any heuristic and merging the two would attach a credential
  nobody asked for -- and a name the gateway does not have is a `warn` in
  `sbx doctor`, which is the only command that both reads the file and can ask.
  `base` goes the other way: the branch a checkout is sitting on is evidence about
  *that* repository and a config entry is a guess about all of them, so it only
  fills a detached HEAD. `repo` moves the picker's *cursor* rather than its
  filter, so every other repository is still one keystroke away, and typing drops
  the preference for good -- the background rescan calls `scanned` again, and
  reapplying it then would pull the cursor off whatever was just filtered for.
  `repo_roots` replaces the conventional places rather than adding to them, and
  `SBX_REPO_ROOTS` still wins over it.

  **A policy that is a path had to be offered in the TUI too.** The form's policy
  field cycled `policy::TEMPLATES`, which cannot represent
  `policy = "./strict.yaml"` -- and a form that quietly fell back to
  `feature-work` would have created sessions under a different policy from
  `sbx new`. The chooser now holds `PolicyOption`s, with a configured path
  prepended and labelled `from your config file`.

  Verified under tmux against a live gateway: the picker opening with the cursor
  on `~/dev/sbx` out of fifteen repositories, the form showing
  `< readonly-explore >` with only the configured provider ticked, a configured
  path showing as its own chooser entry, `sbx doctor` warning about
  `ghost-token`, and every error path above from the command line.

- **22. Acting on a denial from the events feed** — DONE. The feed showed what
  the agent had tried and gave you nothing to do about it; `w`/`t` was the only
  answer and it is one fixed preset. The feed now carries a cursor -- `j`/`k`
  move it, the pane scrolls to follow -- and `e` on the selected event opens a
  four-way question about the endpoint it names: `a` allow here, `b` block here,
  `A` allow always, `B` block always. Lowercase goes through the same live
  `policy update` as `w`; uppercase does that *and* records the endpoint in
  `~/.config/sbx/endpoints.json`, which `ops::create` imposes on every new
  sandbox in one call before the clone starts.

  **A block is a removal, not a veto, and the pane says so.** OpenShell denies by
  default and has no deny-that-outranks-an-allow at L4, so a block list can only
  mean `--remove-endpoint`: blocking `pastebin.com` is a no-op and blocking
  `platform.claude.com` is real, because `feature-work.yaml` grants it. Each list
  row therefore carries a third column read from the live policy -- `NOT in this
  policy`, `STILL in this policy`, `gone from this policy` -- because an entry
  describes what a *new* session gets and the session in front of you may predate
  it. Phrased as the outcome rather than as an absence for blocks: for those,
  absent is what was asked for, and "not in this policy" would make the healthy
  case read as the alarming one.

  **An allow binds to the binary the event named**, which is the whole premise of
  the tool and the one thing a host-level check would get wrong. `github.com:443`
  is granted to git under `feature-work` and denied to curl; answering "already
  reachable" on the strength of git's rule would refuse to fix the exact case the
  feed exists to show. So `App::reachable_by` is binary-aware and
  `App::endpoint_present` is not -- the first is what an allow acts on, the second
  what a block does. An L7 decision names no binary and cannot be allowed at all:
  an endpoint rule with no binaries grants nothing, and the pane says that rather
  than issuing a rule that would quietly do nothing.

  **Asymmetric failure at create time.** A block that will not apply fails the
  create; an allow that will not apply is a warning. A missing allow announces
  itself the moment the agent tries and the feed prints the denial; a missing
  block leaves a session reaching something its owner asked to be unreachable and
  never mentions itself again. Applied after `sandbox create` and before the
  seeder, so the window in which the template's rules are in force is real and
  empty.

  **The cursor is anchored to an event, not to a row.** The feed grows at the top
  and refetches on a timer, so an index is not a handle: two arrivals between two
  keystrokes and `e` acts on something else. `Update::Events` re-finds the
  selected event by `Event::key` -- the same identity the kept file dedupes on --
  and falls back to the newest when it has aged out.

  Measured against 0.0.110 with `policy update --dry-run` rather than assumed:
  `--remove-endpoint` on an endpoint that is not there exits zero and changes
  nothing, so a block never needs a guarding read; an `--add-endpoint` that
  overlaps an existing rule becomes a rule of its own and the CLI says
  `would grant binary '/usr/bin/curl' undeclared authorization for github.com`,
  which is why the pane re-reads the policy afterwards rather than reporting what
  it asked for; a binary-less `--add-endpoint` produces a rule with no `binaries:`
  key at all. Verified live: `sbx policy does-the` rendering the three list states
  against a real `feature-work` sandbox.

- **23. MCP servers, run on the host and reached through the policy** — DONE.
  The agents had no tools beyond their own, and the ones worth having -- Jira,
  Azure DevOps -- need credentials. Running those servers *inside* the sandbox
  would put the credentials inside the sandbox, which is the thing this tool
  exists not to do. So they run on the host in their own containers, holding
  their own secrets, and a `[[mcp]]` table in the config file gives every new
  session one endpoint each.

  **Two topologies, both measured against 0.0.110 with the Docker driver.** A
  sibling container on the gateway's own network (`--network openshell-docker`)
  is reachable from a sandbox *by container name* -- Docker's embedded DNS
  resolves it even though the sandbox has no DNS of its own, because the proxy
  does the resolving -- and publishes nothing on the host at all. A port
  published on the host is reachable as `host.openshell.internal`, which the
  gateway already puts in every sandbox's `ExtraHosts` pointing at the bridge
  gateway (172.18.0.1 here). An IP literal is *not* covered by a hostname rule:
  granting `host.openshell.internal:8931` and then asking for `172.18.0.1:8931`
  is denied, as it should be.

  **The binary is the agent, and that is sharper than the registry rules.**
  Claude Code 2.x is a native binary, so `/usr/local/bin/claude` is a rule only
  the agent satisfies -- unlike npm, whose kernel-resolved exe is `/usr/bin/node`
  and covers everything JavaScript in the sandbox. Demonstrated live in
  `sbx-adoe2e`: `claude -> POST http://mcp-azure-devops:9001/mcp ALLOWED
  [policy:allow_mcp_azure_devops_9001]` beside
  `curl -> ... DENIED [binary '/usr/bin/curl' not allowed in policy]`. This cost
  a round trip to find: with `node` and `curl` on the rule and not `claude`, the
  agent reports the proxy's 403 as **`! Needs authentication`**, which sends you
  looking at credentials for a policy problem. `sbx doctor` exists partly to
  shorten that path -- it says which container is missing or off the network.

  **Registration happens inside the sandbox, before the agent starts.** The
  seeder runs `claude mcp add --scope user` as its own `mcp` step, because the
  agent reads its servers at startup: doing it from the host afterwards would
  leave the first session of every sandbox without tools. `claude mcp add`
  rather than writing `mcpServers` into `/sandbox/.claude.json` by hand, since
  the CLI owns that file's shape and the image already pre-populates it with the
  onboarding keys. The endpoints are opened in one `policy update --wait` at
  create time, next to `impose_lists` and for the same reason: an allow that does
  not land announces itself the moment the agent tries, so it is a warning, not a
  failed create.

  **Loopback is refused when the file is read.** `http://localhost:9000/mcp` is
  correct on the host and means the sandbox itself inside one, so it would look
  fine until an agent was running and then fail as an authentication problem. The
  error names `host.openshell.internal` and the network by name rather than just
  saying no. `stdio` is refused for the same class of reason: it would run the
  server in the sandbox, with its credentials.

  **Streaming is not buffered by the inspecting proxy**, which was the risk worth
  testing before building any of this: an SSE endpoint emitting an event a second
  arrived inside the sandbox event by event, a second apart. Real servers
  verified end to end: `@azure-devops/mcp` 2.9.0 behind `supergateway`
  (stdio-only, so it needs the shim; its `pat` mode wants the base64 of `:<pat>`,
  which it splits on the colon) reported `✔ Connected` to a real session's agent,
  and `ghcr.io/sooperset/mcp-atlassian` answers `--transport streamable-http` on
  `/mcp`.

  **What it costs, recorded rather than glossed.** The gateway sees every MCP
  call as `POST /mcp`, so the method/path rules that make the git endpoints sharp
  buy nothing: the agent gains whatever the server can do, with the host's
  credentials. Fine for Jira and Azure DevOps, whose blast radius is a work item;
  a filesystem or Docker MCP server on the host would be a straight sandbox
  escape, and sbx cannot tell the difference for you.

- **24. Skills carried in, and no attribution stamp** — DONE. Two things a
  sandbox got wrong about being someone's environment rather than a clean room.

  **Skills.** A sandbox has its own `HOME`, so a fresh one has none of them --
  the one part of a setup that did not follow you in. `skills = ["ship-pr"]`
  copies them: a bare name is one of your own under `~/.claude/skills` (or
  `$CLAUDE_CONFIG_DIR/skills`), and anything with a `/` is a path, so a skill
  living in a repository can be named where it is. Packed with `tar -czh` on the
  host, carried as base64 inside the seeder script, unpacked into
  `/sandbox/.claude/skills` as a `skills` step before the agent starts -- the
  whole directory, since a skill is `SKILL.md` beside its scripts and
  references, and a passthrough that moved only the markdown would be worse than
  none.

  **A symlink was the ask and a copy is the answer**, which is worth writing
  down rather than quietly substituting: a symlink does not cross into a
  sandbox, and a bind mount would hand it the rest of `$HOME` -- the isolation
  is the product. What the config file holds is the pointer, which buys the part
  of a symlink that was actually wanted: edit the original, and the next session
  gets the edit. A running session keeps what it was created with, its record
  says so, and the facts pane lists the names.

  **Failures cost the skill, not the session.** A skill that is missing at create
  time is a warning -- computed by re-running the pack in `ops::create`, since
  the seeder runs detached and has nowhere to say it -- and `sbx doctor` reports
  the same three problems (not there, not a directory, no `SKILL.md`) before you
  ever get that far. The 256KiB cap on a packed skill is there because the
  payload rides in an exec argument: over it, the failure would be
  `argument list too long` rather than anything about skills.

  Base64 is fifteen lines in `skills.rs` rather than a dependency, tested against
  the RFC 4648 vectors *and* round-tripped through the real `base64 -d`, which is
  the decoder that actually has to accept it.

  **Attribution.** The baked `claude-settings.json` now sets
  `attribution.commit` and `attribution.pr` to empty strings, which is how Claude
  Code is told to stamp nothing; an absent key means the default trailer, not
  silence. Commits made in a sandbox already carry the host's git identity, so a
  co-author trailer would credit the tool for work attributed to the person
  running it.

  Verified live in `sbx-skilltest`: `step skills` in the state file, the
  `ship-pr` manifest at `/sandbox/.claude/skills/ship-pr/SKILL.md`, and both MCP
  servers `✔ Connected` in the same session -- the first run with real
  credentials rather than placeholders.

  **A window that used to be microseconds became seconds.** Creating a session
  reported `could not adopt sbx-x: cat: /sandbox/.sbx/meta.json: No such file or
  directory` while the session itself came up perfectly. Between
  `sandbox create` returning and the record being saved, the sandbox is an
  orphan: labelled `sbx.managed`, no record, no `meta.json`. Any refresh landing
  there -- the TUI runs one a second -- tries to adopt it and fails on a file the
  seeder has not written yet. `impose_lists` had the same shape, and was
  invisible because an empty list makes no call; `impose_mcp` is a
  `policy update --wait`. The record is now written the moment the sandbox
  exists, before either, so a refresh finds a `creating` record instead of an
  orphan. `read_meta` also grew a `NoMeta` variant, so if it ever does happen the
  message is about the sandbox rather than about `cat`. Verified by racing 25
  refreshes against a create: no warning, and the session seeded through
  `step skills`, `step mcp`, `done`.

- **25. Attach was cooked, not raw** — DONE. A question with options could not
  be answered from an attached session: arrow keys did nothing, Enter did
  nothing, and Ctrl-C was the only key that worked -- which declined the
  question. It read as an agent that had stopped listening.

  **Nothing was putting the local terminal in raw mode.** `openshell sandbox
  exec --tty` allocates a pty at the *sandbox* end and leaves the caller's
  terminal exactly as it found it; measured against 0.0.110 by spawning it under
  a pty and reading the termios back: `ICANON`, `ECHO`, `ISIG` and `ICRNL` all
  still set while the exec ran. Every symptom follows from that one fact. Input
  is line-buffered, so arrow keys arrive in a batch on Enter, if at all, and a
  dialog cannot be driven. `ICRNL` turns Enter into `\n` where the agent's input
  box submits on `\r`, so a typed line sits in the box doing nothing. `ISIG`
  catches Ctrl-C locally, and Ctrl-B never reaches tmux, so detaching is not
  possible either.

  `sbx attach` and the TUI's attach now share `ops::attach_interactively`, which
  holds a raw-mode guard for the life of the child and restores on every path
  out, panic included. Two copies would have been one fixed and one not. A
  terminal that cannot go raw attaches anyway: reading is still worth something.

  Verified under a real pty against a throwaway session: `ICANON/ECHO/ISIG/ICRNL`
  all off during the attach and all back on after it, `echo raw-mode-works`
  delivered keystroke by keystroke with no Enter and run by a bare `\r`, and
  `Ctrl-b d` detaching cleanly with exit 0 -- which matters, since killing an
  `exec --tty` wedges that sandbox's exec path until it is recreated.

- **26. Names that say something, and a task field you can read** — DONE.
  Three small things about the create flow, from using it.

  **Filler spends the name budget.** "I want to add the MaxGaming Scala customer
  id" derived `i-want-to-add`: fifteen characters of wrapper, none of subject.
  `slugify` now drops pronouns, articles, auxiliaries and the wrapper verbs
  (`want`, `need`, `please`), keeping real verbs -- `add the flag` and
  `remove the flag` have to stay two names. A task made of nothing but filler
  falls back to the text as written, since a name is better than no name.

  **The 15-character cap was the gateway's, not ours.** Sandbox names are capped
  at 19 and `sbx-` takes four. So the session name is now ours (40 characters,
  bounded by being a branch and a list column) and the *sandbox* name is derived
  from it: unchanged for short names, and for long ones the first ten characters
  plus four hex digits of FNV-1a over the whole name. Deterministic, because
  `sbx rm` and adoption have to name a sandbox with no record to read it from;
  distinct, because `maxgaming-scala-customer-id` and `maxgaming-scala-tax` would
  otherwise share one sandbox. The full name lives in the `sbx.session` label,
  which has 63 characters. Branches stay `sbx/<name>` and simply get longer.

  **The task field was one row.** A task is a prompt -- a sentence or three --
  and `with_cursor` drew it on a single unbounded line, so past the modal's width
  the text and the cursor were clipped by the border: you could not see what you
  were typing, which is what prompted this. It now wraps over four rows and
  scrolls by whole rows to keep the cursor visible, hard-wrapped at the column
  like the facts pane's task rows so the same text breaks the same way in both.

  **Copy on select, off.** `copyOnSelect` is a Claude Code default-on setting,
  and it is *not* a `settings.json` key -- it is `/config`'s "Copy on select",
  read from the global `.claude.json` the image already writes. Selecting text to
  read it should not take the clipboard, least of all in a terminal borrowed to
  watch an agent. A test asserts the Dockerfile sets it and that `settings.json`
  does not pretend to.

- **27. Toolchains, per session** — DONE. A sandbox that can only clone and read
  can only write code nobody has compiled: the base image has node and python
  because the community image does, and nothing else. `--toolchain dotnet`,
  `--toolchain dotnet,rust`, and a field on the create form.

  **Installed in the image, not in the sandbox.** The same reasoning that decided
  the agent's own version in increment 10: `/usr/local` is not writable by the
  sandbox user and no template lets a sandbox reach a download host, so an agent
  cannot install a toolchain -- and widening the policy far enough that it could
  would hand every session a route to arbitrary tarballs. Each toolchain resolves
  its version from the publisher's release manifest and verifies the download
  against the checksum published beside it (SHA-512 for the .NET SDK, SHA-256 for
  Rust), then verifies what it installed with `--version`, exactly like the Claude
  Code step.

  **One image per set, layered onto the base.** `sbx-base:dotnet`,
  `sbx-base:dotnet-rust`, `FROM sbx-base:latest`. Docker shares the base's 5.17GB,
  so `sbx-base:dotnet` costs 0.8GB and a Rust session never carries the .NET SDK.
  The tag is a pure function of the *set* -- `TOOLCHAINS` order is imposed on the
  input -- so `--toolchain rust,dotnet` and `--toolchain dotnet,rust` are one
  image rather than two identical ones. Built on first use by `sbx new`, never by
  the TUI, for the reason the base image is not: the build streams docker's output.

  **A toolchain is also a policy change**, which is the half that makes this a
  module rather than a longer Dockerfile. `net-open.yaml` had already argued it:
  it refused to ship crates.io because "the sandbox image ships no Rust
  toolchain, so the endpoint would be unreachable decoration. Add it alongside a
  cargo binary if the image ever grows one." So the registry travels with the
  toolchain -- `index.crates.io` and `static.crates.io` for cargo,
  `api.nuget.org` for dotnet, `registry.npmjs.org` for node -- `read-only`, and
  imposed on the session that asked, one `policy update` per distinct binary list
  so cargo cannot reach nuget.

  **rustup was the wrong tool, for a reason only this gateway shows.** Its
  `cargo` is a proxy that execs
  `$RUSTUP_HOME/toolchains/<channel>-<triple>/bin/cargo`, and the gateway matches
  the kernel-resolved `/proc/<pid>/exe` -- the same trap `net-open.yaml`
  documents for uv's managed python, where `pip install` is denied naming a path
  nobody put in the policy. The standalone distribution lays down a real binary
  at a path this project chooses, so the rule can name
  `/usr/local/rust/bin/cargo` and be right on every architecture. Verified: that
  is exactly what `readlink -f $(command -v cargo)` reports in the built image,
  and `/usr/local/dotnet/dotnet` for the .NET muxer behind its symlink. A test
  checks every rule's path against the layer that installs it.

  **The environment had to go through tmux.** `ENV` in an image does not reach
  the agent -- the gateway does not pass it through to an exec, which is why the
  seeder exports the locale by hand -- so `CARGO_HOME`, `NUGET_PACKAGES`,
  `DOTNET_CLI_HOME` and the two telemetry opt-outs are appended to
  `/etc/tmux.conf` as `set-environment -g`, the way the base image already
  handles `LANG`. Verified in a pane. Everything a build writes lands under
  `/sandbox`; `/usr/local` stays read-only to the agent, so it cannot replace its
  own compiler.

  **The form arrives filled in**, in the spirit of increment 15: `repos::inspect`
  now also reads the checkout for markers -- `Cargo.toml`, `package.json`, a
  `.csproj` one level down -- and the create form ticks what it finds, skipping
  build output and vendored dependencies, which contain every marker there is.
  A tick changed by hand survives an answer that arrives afterwards.

  **Verified against a live gateway**, which is where the last piece came from.
  `cargo fetch` and `dotnet add package` both work through the gateway; `curl`
  and `node` reaching the same hosts are denied at the CONNECT tunnel with a 403,
  and the feed names the binary and the rule. The dotnet restore also left six
  denials nobody would want -- NuGet checking its *signing* certificates against
  `crl3.digicert.com`, `ocsp.digicert.com` and `www.microsoft.com/pkiops/crl` --
  which is a soft-fail check the restore does not need. Allowing three more hosts
  or not making the request: `NUGET_CERT_REVOCATION_MODE=offline` is the second,
  and the same restore now leaves ten allows and no denials at all.

  **`sbx doctor` says what each variant carries**, read from a manifest the
  layers write inside the image rather than inferred from the tag, and warns when
  a variant is older than the base it sits on -- rebuilding the base for a newer
  agent leaves the variants on the old one, and nothing about that looks wrong
  from outside.

### Later, unscheduled

- **Warm pool** — less urgent than expected: sandbox creation is ~1s with the
  image cached, and cloning dominates. Prewarming the *clone* would help more
  than prewarming the sandbox.
- **Port forwarding** — `openshell forward` and `openshell service` for dev
  servers an agent starts.
- **Recovering a wedged sandbox** — after an abruptly killed attach, exec hangs
  forever for that sandbox. `sbx doctor <session>` could detect it (exec with a
  short timeout) and offer `sandbox stop && sandbox start` as a repair before
  falling back to recreating.
- **More toolchains** — go, a JDK, and the build tool each one implies (`go` is
  the cleanest: one tarball, a real binary, `proxy.golang.org`). A toolchain is
  now a table entry in `toolchain.rs` plus a Dockerfile fragment, and the tests
  there check the three halves against each other.
- **Multiple agents** — the `agent` field is already stored per session and
  hardcoded to `claude`. codex, opencode and copilot are all in the community
  policy, so most of the work is policy templates plus a launch command.
- **A local repository as a *source*** — `openshell sandbox upload` would let a
  session start from unpushed work, or from a checkout with no remote at all.
  See increment 9 for why it clones `origin` instead, and what it would cost.

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
- **The OCSF log format is not an API either.** The events pane parses
  `[ts] [source] [level] [logger] CLASS:ACTIVITY [SEV] ...` by hand, because
  `openshell logs` has no JSON output. Mitigated the same way as the pane
  scraping: one module, real captured specimens in its tests, and lines that
  fail to parse are dropped rather than crashing the pane.

  Increment 22 raised the stakes: a subject is now turned back into an endpoint
  and one keystroke away from a rule at the gateway. `Event::target` is therefore
  strict rather than forgiving -- an authority needs a dotted host and a real
  port, an L7 subject needs exactly two words with an uppercase method first, a
  binary needs an absolute path -- so a log line whose shape has changed yields
  *nothing* and the pane says the event is not about an endpoint. A loose parse
  here would be a policy change nobody asked for, which is worse than a feed that
  has stopped being actionable.
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

Current state: increments 0-22 done, `main` at a clean tree, 338 tests, clippy
and rustfmt clean. `sbx doctor` should be all green; if the gateway is down,
`systemctl --user status openshell-gateway`.

The loop that works today, end to end:

```sh
sbx config --init   # optional: put the flags below in a file and stop typing them
sbx new --repo <url> --task "..." --policy feature-work \
        --provider claude-oauth --provider azure-pat
sbx            # or start one here: n, pick a repo, fill the form, enter
               # Enter to attach, Ctrl-b d to detach, q to quit
               # Tab cycles preview/diff/policy/events; w/t widen/tighten egress
               # in the feed, j/k pick an event and e allows or blocks its endpoint
               # P publishes (asks first)
sbx publish <name>
sbx rm <name>
```

Providers are per organisation, not per forge: an Azure DevOps PAT only covers
the org it was minted for, so a work repo and a personal one need one each.
`openshell provider list` shows what exists; the profiles are checked in under
`providers/` and registered with `openshell provider profile import --file`.

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
* **`XDG_CONFIG_HOME` cannot be used to isolate `sbx` in a test.** `openshell`
  reads it too, so pointing it at a temp directory hides the registered gateway
  and `sbx doctor` fails with `could not parse ... missing field 'gateway'` --
  which looks like the gateway being down and is not. Fine for exercising
  `sbx config` and its error paths, which need no gateway; for anything else,
  write the real `~/.config/sbx/config.toml` and delete it afterwards.
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
* **The gateway will report a policy it is not enforcing.** Only the network and
  inference sections of a live sandbox's policy actually change; a filesystem or
  process change is accepted, acknowledged as loaded, and returned by
  `policy get --full` while Landlock goes on enforcing the creation-time
  ruleset. Never trust those two sections on a running sandbox -- check the
  `Applying Landlock` line in `openshell logs` for the counts really in force.
* **`access:` and `rules:` on one endpoint grant the union, not the
  intersection.** `rules:` alone is default-deny; adding an access class next to
  it re-allows everything in that class. A policy can therefore read far
  stricter than it is. See increment 7.
* **A provider does not hand the sandbox a secret; it hands it a placeholder.**
  The env var contains `openshell:resolve:env:v<id>_<NAME>`, and the gateway
  swaps the real value into an outgoing header that already contains it --
  including inside the base64 of a Basic credential. It never adds a header on
  its own, so a request that sends no `Authorization` is simply unauthenticated.
  This is why git needs `http.extraHeader` and why that config is safe to
  persist. See increment 8.
* Azure DevOps PATs are Basic-with-the-token-as-password, and a wrong auth
  style answers 302-to-a-sign-in-page rather than 401. If a forge ever looks
  like it is "silently ignoring" credentials, check the auth style before
  anything else.
* Anything that reads the sandbox costs an exec, and an exec is itself five
  OCSF log events. That is fine until something *reads the log* -- see the
  filter in `crates/sbx/src/events.rs`, and expect the same problem in any
  future feature that watches the gateway's own output.
* **A held `exec --tty` does not block ordinary execs, and killing one does not
  wedge the sandbox.** (Measured for increment 11's embedded terminal, which
  increment 13 removed; the finding still stands and still matters for anything
  that holds an exec open.) Both were assumed to be true the other way round -- the
  second is written down in increment 0 -- and both were measured on 0.0.110
  while building increment 11: execs stayed at ~200ms with an attach open, and
  survived the attach being killed. Worth re-measuring rather than trusting if
  the embedded terminal ever starts feeling slow, because everything it does
  rests on it.
* **The agent is on the alternate screen, so nothing outside it has a
  scrollback.** `#{alternate_on}=1`, `#{history_size}=0`: tmux keeps no history
  for that pane and a host-side parser has none to collect either, because the
  pty carries repaints rather than scrolling lines. Anything that wants to scroll
  an agent has to let the agent do it. Check those two format strings before
  building a scrollback for any agent, and expect a different answer from one
  that renders inline. See increment 11.
* **A create that dies leaves a half-cloned sandbox, and it looks like one still
  working.** *(Fixed in increment 19 by moving seeding into the sandbox; the note
  stays because the shape of the problem recurs -- anything the host does *to* a
  sandbox over a single exec dies with the host.)* The create thread is detached -- quitting the TUI mid-clone is enough
  to kill it -- and what is left is a `.git` with a partial pack: 69MB of a 238MB
  repository, `count-objects` reporting zero, `rev-parse HEAD` failing. Nothing in
  the gateway log, because nothing failed; the client simply went away. Increment
  18's repair pass makes the *record* honest, but the sandbox is still unusable and
  the only cure is to destroy it. The durable fix is to run seeding *inside* the
  sandbox, detached from the exec that starts it, so it survives the tool that
  asked for it -- which is the same principle as the agent's own tmux session.
* **A tmux client with no locale is not a UTF-8 client, and the gateway strips
  the environment.** Two facts that only bite together: the image's `ENV` never
  reaches an `openshell sandbox exec`, so a tmux client started there falls back
  to the DEC line-drawing set and turns every glyph it cannot map into `_`. Any
  tmux invocation that talks to a terminal wants `-u` and an exported locale, and
  anything that starts the tmux *server* has to set it too, because panes inherit
  the server's environment. See increment 14.
* **tmux keeps a window at its last client's size after that client leaves.**
  `default-size` only applies at creation, so anything that attaches at a small
  size -- an embedded pane, a narrow terminal -- decides what the status scraper
  reads from then on. Whatever attaches has to put the size back.
* **The agent's version is the image's problem, and pane markers belong to a
  version.** The base image freezes whatever Claude Code was current when it was
  published, and a sandbox can neither reach the download service nor write to
  /usr/local/bin. Worse, upgrading moves the markers `status.rs` matches on: by
  2.1.246 the footer is a truncated list of rotating hints, so `? for shortcuts`
  is only sometimes present. Anything that reads the agent's screen has to be
  re-verified against a real session after a version bump -- and the pane it
  reads has to be wide enough that the markers are not truncated away. See
  increment 10.
* `openshell logs` and `policy list` have no `--output json`. The log is parsed
  by line in `events.rs`; policy history is not surfaced at all because the
  table would have to be scraped.
