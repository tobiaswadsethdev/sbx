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

---

# Pivot: the ADE

Increments 0-22 built a terminal UI for one person on one Linux box. The pivot
keeps every part of that -- the sandbox per session, the per-binary policy, the
allow/deny feed, the credentials the sandbox never sees -- and puts a desktop
application in front of it, with the server free to be somewhere else.

The reference for the shape is [Orca](https://github.com/stablyai/orca): a fleet
of parallel agents, a task inbox wired to the trackers, a real terminal, diffs
you can annotate. What Orca isolates with a git worktree, this isolates with a
kernel-enforced sandbox, and the policy and events panes are the part no ADE
has.

## Why the code is ready for this

`ops.rs` -- the operations both the CLI and the TUI already share -- imports
nothing from ratatui. `openshell-client` is one trait. Sessions describe
themselves from inside their own sandbox, so a client dying is a non-event and
a *second* client is nearly free. The headless core is mostly already written;
it is just not a crate yet.

The exceptions, and the work they imply: `ansi.rs` returns ratatui `Span`s,
`pane.rs` is presentation, and `policy.rs` builds a pane body. Anything that
renders has to move behind the boundary, leaving the core returning structured
values that a terminal and a web view can each draw their own way.

## Locked decisions

| Decision | Choice | Why |
| --- | --- | --- |
| Desktop stack | Tauri v2 | Reuses the Rust core and its serde types; the Windows binary is WebView2, so it is ~10MB and Chromium-backed, which is what a WebGL terminal wants. Linux desktop gets WebKitGTK and a rougher terminal -- accepted, because the Linux user already has the TUI |
| Transport | A listening `sbxd`, TLS + bearer tokens | Chosen over stdio-over-SSH. Costs a certificate and pairing story, buys multi-client, a mobile client later, and a server that does not need an SSH account per user |
| Backends | Sandboxed *and* unsandboxed worktrees | The sandbox stays the default and the point. A worktree session runs on the server with the server's rights, and is labelled as such everywhere it appears |
| The TUI | Frozen | Bug fixes only. It stays building against the core, which is the cheapest possible test that the core has not grown a UI dependency |
| UI data | Structured, never markup | The core returns `PolicyView` and `Vec<Event>`; the TUI makes lines out of them and the web view makes elements. Shipping pane markup over the wire would make the desktop app a screen-scraper of the terminal one |

## Shape

```
crates/
  openshell-client/   unchanged -- the trait
  sbx-core/           ops, session, store, policy, events, seed, skills, mcp,
                      publish, repos, image, toolchain, config, doctor, status
  sbx-proto/          the wire types, one serde definition, TS generated from it
  sbxd/               the server: axum, TLS, /rpc + one multiplexed websocket
  sbx/                the clap CLI and the frozen TUI, in-process on the core
apps/desktop/         Tauri v2 -- the only thing that ships to Windows
```

`sbx-proto` is the single definition of the protocol and `ts-rs` emits the
TypeScript from it into `apps/desktop/src/gen/`, checked current in CI. Two
hand-maintained copies of a message type is the failure mode that makes a
self-hosted client-server product miserable, and it is avoidable for the cost of
a build step.

### The server

One TLS listener. `POST /rpc` for request-response, one multiplexed websocket
for everything that streams -- the events feed, agent status, and the PTY --
because a single connection is one token check, one reconnect path, and one
thing to notice has dropped.

`GET /version` is unauthenticated and carries a protocol integer. A desktop app
and a self-hosted server *will* drift, and a client that can say so beats a
client that fails strangely.

**Binding to anything but `127.0.0.1` is explicit.** An authenticated `sbxd` can
create containers on its host, which makes it equivalent to root there; that
belongs in the docs and in the warning the flag prints, not in a footnote.

### Pairing

`sbxd` generates a self-signed certificate on first run, with the hostname, the
local addresses and `localhost` in its SANs. `sbxd pair` prints one connection
string -- `sbx://host:port/<token>#<cert-fingerprint>` -- and the QR code that
the same string becomes useful as when there is a mobile client. The desktop app
takes one paste. The client pins the fingerprint on first connect and refuses a
changed one afterwards; tokens are stored hashed, named, and revocable with
`sbxd token rm`.

### The WSL case, which is the sharp one

The whole point of the Windows story is a server inside WSL and a UI outside it,
and whether `localhost` reaches across depends on whether WSL2 is in mirrored or
NAT networking mode. `sbx doctor` on the WSL side should detect which, and print
the address Windows should actually use -- including the `netsh portproxy` line
when it is NAT. Getting this wrong looks exactly like a firewall problem and is
not one.

## Two backends

A `Backend` trait behind `ops`, with the openshell path as one implementation
and a `git worktree` path as the other:

```rust
trait Backend {
    fn create(&self, spec: &SessionSpec) -> Result<Placement>;
    fn exec(&self, s: &Session, cmd: &[String]) -> Result<Output>;
    fn attach_pty(&self, s: &Session) -> Result<PtyHandle>;
    fn destroy(&self, s: &Session) -> Result<()>;
    fn isolation(&self) -> Isolation;   // Sandboxed { policy, events } | None
}
```

Three things a worktree session cannot have, each of which has to be *said*
rather than left blank:

* **No policy pane.** It reads "no isolation -- this session runs on the server
  with your rights", not an empty box that looks like a loading failure.
* **No events feed.** There is no gateway deciding anything, so there is nothing
  to allow or block.
* **A different publish.** `publish.rs` pushes from *inside* the sandbox
  precisely so the token never reaches the host; a worktree push uses the
  server's own git credentials. Same button, materially different guarantee.

The list badge says which kind a session is. A product whose pitch is isolation
cannot have a mode where the isolation is quietly absent.

The source-of-truth invariant also bends here: there is no sandbox to hold
`meta.json`, so a worktree session's record lives in the server's state
directory -- not in the worktree, where it would show up in every `git status`
the agent runs. Adoption after cache loss becomes `git worktree list` reconciled
against that directory.

## Skills and MCP, now that there are two hosts

"The host" used to mean one machine. It now means the server, while the skills
and the muscle memory live on the machine with the UI.

**Skills** get a server-side library at `$XDG_DATA_HOME/sbx/skills/`, filled
from two sources: server-local paths, exactly as the config file does today, and
uploads pushed by the desktop client from its own `~/.claude/skills`. The
pointer-not-copy property survives -- the client re-uploads on create, so editing
the original still means the next session gets the edit -- and a session still
records precisely what it was given.

**MCP servers** stop being a documented `docker run` incantation and become
something `sbxd` owns: a catalog of name, image, args, environment and transport;
containers started on `openshell-docker` and health-checked; secrets in a
server-side store that never travels to a client. `sbx doctor`'s MCP check turns
into live status in an Integrations screen, and the per-binary grant is unchanged.

The warning in `docs/mcp.md` moves into the UI, at the moment a server is ticked
for a session rather than in a document nobody re-reads: the agent gains
everything that server can do, the gateway sees only `POST /mcp`, and a
filesystem or Docker MCP server is a straight sandbox escape.

## The task inbox

GitHub, Azure DevOps and Jira, read server-side over REST with the credentials in
the server's store -- REST for what the *UI* shows, MCP for what the *agent*
gets. They are different consumers and conflating them makes both worse.

`forge.rs` already knows which host a repo belongs to and `publish.rs` already
opens pull requests on two of them, so the new part is the reading and the round
trip: open a session from a ticket with the task, base branch and a branch name
following `name/PROJ-123-description` already filled in, and on publish, comment
the PR link back and move the ticket. That loop currently exists as a personal
skill; it is the thing an ADE should do with a button.

## UI

* **Left** -- repositories, then their sessions, each with a state badge, the
  waiting count, and the isolation kind.
* **Centre** -- per session: Agent (xterm.js on the websocket), Diff, Files.
* **Right** -- Facts, Policy, Events. The events feed keeps the TUI's best
  interaction: pick a decision, allow or block that endpoint, one keystroke.
* **Top** -- the task inbox.

Two things the desktop gets that the terminal could not. **An OS notification
when a session starts waiting on a permission prompt** is the single largest
quality-of-life gain here; watching several agents is exactly the case where a
terminal loses. And **inline comments on a diff, batched and sent back to the
agent**, which is review as a conversation rather than a re-prompt.

The terminal is a place this is better positioned than the reference. tmux
already runs *inside* the sandbox, so xterm.js over the websocket to
`openshell exec --tty` gets scrollback and reconnect across a dropped network
for free, with nothing persisted client-side.

## Increments

- **23. Headless core** — DONE. `sbx-core` holds the twenty modules that do not
  draw; `crates/sbx` keeps the clap CLI and the frozen TUI. No behaviour changed:
  the same 408 tests pass, now 259 in the core and 149 in the binary, and
  `sbx doctor`, `policies`, `toolchains` and `config` were run against the live
  gateway afterwards to check that the embedded policy YAML, Dockerfiles and
  `config.example.toml` all survived moving a directory.

  Two things crossed the line and had to move. `ansi.rs` returned ratatui
  `Span`s, so it now tokenizes into a `Style`/`Color`/`Modifiers` of its own --
  serde-derived, because a captured screen is something a client will be sent --
  and `tui/ansi.rs` maps that onto ratatui. And `ops::attach_interactively` put
  the local terminal into raw mode through `ratatui::crossterm`, which is not the
  core's business: it moved to `attach.rs` in the binary, where both callers
  already live, so it is still one definition rather than two. `ops` keeps
  `attach_script`, which is the same shell wherever it is run from -- and the
  long comment explaining that script, which had drifted onto the caller, is now
  on it.

  **The plan said `pane.rs` moves to the TUI, and the code says otherwise.**
  `pane.rs` is markup in a `String` with no UI dependency at all, and it has
  three consumers, not one: `policy.rs` builds a body with it, `ops.rs` shares
  its sigils for the diff, and `sbx policy` prints `to_plain` to a pipe. Moving
  it into the TUI would have dragged two core modules and a CLI command along
  behind it. It stays in the core.

  What that markup *is* -- a serialised pane, parsed back by whoever draws it --
  is still wrong for a wire protocol, and the `PolicyView` this deferred is real
  work. It belongs in increment 24, where `sbx-proto` will say what shape the
  structured version actually needs to be. Designing it now, against no consumer,
  would have been guessing.
- **24. `sbx-proto` and `sbxd`** — DONE, apart from the two halves that turned
  out to want a UI first; see below. The wire types, the server, TLS, tokens,
  pairing, `/rpc`, and `sbx` itself as a client of it. `sbx doctor` learns the
  paired servers and the WSL networking modes. 485 tests.

  The types on the wire are the core's own rather than a second set of DTOs.
  That couples the protocol to the core's structs -- renaming a field is a
  protocol break -- and `VERSION` behind an unauthenticated `/version` is what
  makes the break loud. A second definition would only have moved the coupling
  somewhere a compiler cannot see it.

  Errors are an envelope and not a status, because a request that failed for a
  reason the client should act on is not a transport failure: the round trip
  worked. A status is kept for what really is transport. The one in between is
  an `op` an older server has never heard of, which comes back as `unsupported`,
  because a client can explain that and cannot explain a 400.

  Pairing is `sbx://host:port/<token>#<fingerprint>`, and the fingerprint is the
  part that matters: it means the *first* connection is verified too, which is
  the hole in ordinary trust-on-first-use. The client checks it and nothing
  else -- deliberately not the hostname, since the fingerprint answers a stronger
  question and a name check would only break a server reached at an address that
  is not in its certificate, which is the WSL case exactly.

  **Building `sbx --server` before any UI paid for itself three times**, in ways
  the type checker could not have found. `--server ls` parsed as a server called
  `ls` and fell through to the TUI. `TcpStream::connect` has no timeout, so the
  read and write timeouts set immediately after it -- and the comment claiming
  they covered a port with nothing on it -- were both wrong. And the token set
  was read once at startup, so `sbxd revoke` did nothing until someone restarted
  the server, which is the opposite of what revoking is for.

  **Two parts moved out, because they wanted a consumer first.** The multiplexed
  websocket carries the events feed, agent status and the PTY, and every one of
  those is shaped by what the UI does with it -- the PTY especially, which is
  increment 26's whole subject. The TypeScript generation wants a UI build to
  generate into. `PolicyView` moved with them for the same reason it was deferred
  out of 23: `Reply::Policy` currently carries the revision and the global lists,
  which the CLI renders with the same code the TUI uses, and the structured
  version should be designed against the thing that will draw it.
- **25. The shell** — DONE. Tauri v2 and React, the session list, and
  facts/policy/events read-only, against a live `sbxd`. Carried what increment
  24 left with it: `policy::View` and the generated TypeScript.

  `sbx-client` is its own crate now, because the desktop application needs the
  same connection the CLI makes and a webview cannot make it -- `fetch` has no
  say in which certificate it will accept, so pinning has to happen on the Rust
  side of Tauri. The webview never speaks to the server at all.

  `policy::View` replaced the marked-up string the pane used to be. That also
  took `openshell-client` off the wire, which is worth more than it sounds: a
  protocol pinned to a `0.0.x` project's types has that project's churn as
  protocol churn.

  **The generated types earned their keep the first time they were used.**
  `State` is lowercase on the wire and `Verdict` is PascalCase -- an
  inconsistency that has to stay, because events are persisted as JSONL and a
  rename would make every file on disk unreadable. The hand-written
  `e.verdict === "denied"` compiled, ran, and would have painted every denial as
  neutral. Generated, it was a type error.

  `apps/desktop` is deliberately not a workspace member, so `cargo build
  --workspace` does not need a GUI toolkit installed to check that a session
  store reconciles.

  **Most of the time this took went on a bug that did not exist.** A development
  build loads the frontend from Vite's dev server; running the binary directly
  without that server gives a window reading `Operation was cancelled`, then a
  blank one, which reads exactly like a broken frontend. Three fixes went in for
  it -- stripping `crossorigin` from Vite's tags, disabling WebKit's DMABUF
  renderer, and disabling WebKit's sandbox -- each with a confident comment
  explaining why it was necessary. Tested afterwards against the working path,
  none of them were, and all three are gone. The sandbox one is the one worth
  remembering: a security-relevant change, made on a guess, documented as though
  it had been established.

  Two things from that detour stayed, both real. Debug builds open the web
  inspector, which is the only way to see a console message from inside that
  window. And a screenshot has to name the window id: `x11grab` on a region of
  the screen returns solid black for a redirected window, which is what made the
  first captures lie about what was on screen.
- **26. Terminal** — DONE, drawing included. The multiplexed websocket, the
  events, status and terminal channels, `sbx-client`'s streaming half,
  `sbx watch`, and the pane. 501 tests plus three `#[ignore]`d live ones.

  One socket, several channels, JSON frames so a connection stays readable in a
  log -- terminal bytes base64 inside them, because a pty read lands wherever it
  lands and a split multi-byte character cannot go in a JSON string. The server
  polls and sends only what is new, which is the point of it: a client asking
  `/rpc` every second would spend a handshake per session per second to hear
  that nothing had changed, and two clients would double the load on a sandbox.

  **The terminal needs a pty on the server's side as well**, which took
  measuring to find. `openshell exec --tty` gives the sandbox process a real
  terminal -- `tty` reports one, `test -t 0` succeeds -- but the CLI will not
  proxy interactively through pipes: with stdin closed it writes tmux's redraw,
  with stdin an open pipe it writes nothing, ever, sent to or not. `TERM` made
  no difference; stdin was the whole variable. So the child is spawned into a
  local pty as a terminal emulator would, which is what
  `interactive_exec_argv` has always been shaped for. Resizing then becomes the
  pty's own, which also deleted a deadlock: the `tmux resize-window` exec it
  replaced was awaited inside the read loop, and execs are serialised per
  sandbox, so it could wait on the attach that was holding the path.

  Closing a channel detaches with `Ctrl-b d` rather than killing the exec, since
  killing one wedges the exec path for the whole sandbox. There is a live test
  that opens a terminal, closes it, and opens it again, because a test that
  opened one would pass either way.

  **xterm.js did not paint under WebKitGTK**, and the cause was not in the
  stream: the bytes reached the buffer -- `getLine(1)` read Claude Code's
  banner -- and the character cell measured zero, which is a renderer that
  skips. WebKit reports a font's bounding box as zero through a canvas, and
  xterm picks its canvas measuring strategy on whether those properties exist
  rather than whether they answer, so it never falls back to measuring the DOM.
  `src/charSize.ts` probes the canvas and hides `OffscreenCanvas` for the length
  of `Terminal.open` where it cannot measure, which forces the fallback. Drawing
  then revealed a second fault with the same cause: WebKit puts the baseline
  five pixels higher for `line-height: 17px` than for `line-height: normal`,
  though the two are the same seventeen pixels, and rows are `overflow: hidden`
  -- so the top of every line was shaved and `README` read as `KEADME`. The
  rows' spans are put back to `line-height: normal`, which is the only setting
  consistent with a cell height that was measured that way. Both written up in
  docs/desktop.md, and both worth reporting upstream.

  Seven rounds went into that last hop, and the test that finally separated
  "the emulator cannot draw" from "the stream is wrong" was writing one literal
  string into xterm -- which was identified as the right first move several
  rounds before it was run.
- **27. Create** — DONE. The picker and the form as a GUI, and the protocol's
  first write: `Repos`, `Inspect`, `NewOptions` and `Create`. 509 tests.

  Two stages, like the TUI's and for the same reason: which repository is a
  search, what kind of session is a handful of fields with defaults good enough
  to submit on sight.

  **The repositories are the server's.** A checkout only ever *names* a remote,
  but which checkouts exist is a fact about the machine that will do the
  cloning, and `repo_roots` is configured there -- so a window pointed at a
  server elsewhere lists that server's repositories rather than a set of paths
  it cannot reach.

  `Create` answers when the request is accepted, not when the agent is running.
  The states a create passes through are already on the session and already
  polled, so a request that waited would hold a connection open for a minute to
  say what the list was about to say anyway; what it does do before returning is
  everything that can be judged from the request, so an unknown toolchain or a
  name that is not a name fails against the request that caused it. The image
  build moved onto that thread, since the reason it sits in `sbx new` rather
  than in `ops::create` is that it streams docker's output to a terminal, and a
  server has none.

  Three decisions kept out of the webview on purpose, all because a second
  implementation is a second answer: the name is derived by the server from the
  same `derive_name` the command line uses when the field is blank; the
  credentials are ticked by `preselect_providers`, which moved out of the TUI
  into the core when this form needed the same answer -- caught by looking at
  the finished form and seeing nothing ticked, which would have created sessions
  whose agent comes up to a login prompt; and skills and MCP servers are read
  from the server's config by `into_draft` rather than from the request -- so a client cannot attach a tool, or the endpoint the
  policy then opens for it, by asking for one. `NewSession` exists rather than
  `Draft` on the wire for that reason.

  The one place the two front ends differ is the picker's filter, which matches
  substrings where the TUI ranks with `repos::score`. Reimplementing the scorer
  in TypeScript would be the copy this whole crate layout exists to avoid, and
  the alternative is a request per keystroke. Written down in docs/desktop.md
  rather than left to be discovered.
- **28. Diff** — DONE. The three sections as a pane, and a review that goes to
  the agent rather than to a pull request. 520 tests.

  The body is the same marked-up text `sbx new`'s TUI draws, so this is the
  second renderer of the `pane::SECTION`/`NOTICE` contract rather than a second
  format. Line numbers come from the hunk headers, counted forward as git wrote
  them, which is what lets a comment name a line at all.

  **A review is one message, sent once.** Six remarks delivered as they are
  written would interrupt the agent six times, and the second interruption lands
  while it is acting on the first. `ops::tell` uses `load-buffer` +
  `paste-buffer -p` rather than `send-keys` for the same reason at a smaller
  scale: `send-keys` types a multi-line message a key at a time, so every
  newline in it is a submission. A bracketed paste is one block of text, and the
  single `Enter` after it is the submission.

  Kept on the server, per session, beside the events feed -- a client is a
  window onto a session, and a review half-written when the window closes is
  work. Cleared only after the paste lands, so an unreachable sandbox costs the
  delivery and not the review. What is stored is the remark plus the line it was
  written against, verbatim: the working copy moves under a review, and a
  comment that tried to follow a line through later edits would either be wrong
  or need a diff of the diff.
- **29. The workspace shell** — DONE. Projects, the tree, tabs and the dock.
  525 tests.

  The flat session list was right while a session was the unit of work and
  stopped being right at four repositories: a list sorted by name says nothing
  about which four. So the window is shaped like an editor -- projects contain
  worktrees, a worktree contains what you have open in it, and the dock carries
  the two things an ADE built on git worktrees has no equivalent for.

  **A project is a decision, not a discovery.** `repos::discover_in` finds every
  checkout on the machine; a project is the handful someone has said they are
  working on, and it is created by picking one. Stored rather than derived,
  because a project with no worktrees yet is the normal state of one you just
  made and no amount of grouping sessions by clone URL can represent it. A
  worktree records its project rather than being matched back by URL: two
  checkouts of one repository is a normal thing to have, and it would otherwise
  belong to both. `sbx new` has no projects, so what it creates is grouped by
  clone URL at the bottom of the tree rather than hidden.

  The picker moved out of the create flow and into project creation, which is
  the point: it was the first question of every create and it is now a standing
  answer, so what is asked when starting work is the part that varies.

  Tabs are per worktree and stay mounted, hidden rather than unmounted -- a
  terminal that unmounts closes its channel and detaches. The dock is not a tab
  on purpose: a denial you have to open a tab to find is one you will not find.

  One bug the restructure introduced and the app caught: the form stopped
  sending a branch to `Inspect`, since a project stores a path and not a
  checkout. `base_on_remote` is `branch.is_some_and(..)`, so every branch
  reported as missing from the remote and the form fell back to the remote's
  default without saying so. Resolved on the server now, which is what `None` on
  that request always claimed to mean.

- **30. Files, and the editor** — DONE. `Files` and `File`, the tree under the
  project tree, and Monaco. 531 tests.

  Read-only, because the agent owns the working copy: two writers with no shared
  lock is how a file ends up with half of each. One directory per request as the
  tree is expanded -- a repository is tens of thousands of files and each
  listing is an exec -- and collapsing forgets a level, so reopening re-reads
  what the agent has done since. Paths are checked by component on the server;
  contents come back base64, since an exec's stdout is already lossy UTF-8 and a
  source file with a stray byte would come back altered.

  Monaco was measured in WebKitGTK before anything was built on it, which is the
  lesson from the terminal applied rather than restated. It renders: character
  width 8.4 where xterm's canvas path returned zero. **But it computes its diff
  in a web worker and fails quietly without one** -- the editor still draws and
  the diff editor shows two panes with no red or green, which reads as an empty
  diff. Caught only because the probe counted decorations rather than trusting
  the screenshot: three with a worker, zero without.

  Importing `monaco-editor` whole also brings the language services, four more
  workers and a 15MB bundle, to power completions in a viewer that cannot be
  typed into. The editor API plus `basic-languages` is 4MB and keeps the one
  worker that matters.
- **31b. Icons, and the same two pixels again** — DONE. Inline SVG icons, file
  icons by kind, and the font-metrics correction applied to the window rather
  than only to the terminal.

  The clipping fixed in increment 26 was never terminal-specific and was fixed
  as though it were. WebKit puts the baseline about two pixels too high for
  *any* explicit `line-height`; the body sets `1.5`, so every element that clips
  lost the top of its text and `NOTES.md` read as `NOIES.md` -- legible enough
  to look like a font rather than a bug, which is how it survived a whole
  increment of looking straight at it. One rule on `body`, keyed on the class
  the probe already sets, fixes all of them.

  Icons are drawn here rather than pulled in: an icon set is a package of a
  thousand glyphs to use fifteen, each with its own stroke weight. Monaco does
  bundle codicons and reusing them was the obvious alternative -- it would tie
  the window's chrome to a version of an editor it happens to embed. The file
  icons cover the kinds you actually scan a directory for and nothing else; two
  hundred extensions is two hundred chances to be subtly wrong, and an unknown
  one gets the same page outline rather than nothing.

- **31a. Git, and the diff in the editor** — DONE. `sbx_core::git`, the dock's
  git view, and Monaco's side-by-side diff replacing the unified text pane. 539
  tests.

  Full staging, so `Status` is the index and the working copy as two lists and a
  file edited, staged and edited again appears in both -- which is the case
  staging exists for and the one a single list cannot show. The status parser is
  pure and tested against git's own output: the two columns, a conflict as one
  entry rather than two, a rename under its new name, and a quoted path
  unquoted.

  **The agent is editing while the view is on screen**, and that shapes all of
  it. A status is a snapshot already out of date; staging records the version
  that exists at that moment; discarding races whatever the agent is writing.
  Git's index is the only lock there is and the agent does not take it, so the
  view never pretends otherwise: every action re-reads the status from the
  server rather than adjusting the list it had, and reports git's own words.
  `pull` is `--ff-only` and `push` is always `-u`, so a branch that has never
  been pushed has an upstream to measure against afterwards -- which is why the
  button says `publish` until it does.

  The review moved into the diff editor and nothing about it had to change:
  comments have always stored `{file, line, excerpt}`, which is already per
  file. That is the reward for storing the excerpt rather than a line identity
  -- the anchor never depended on which rendering it was written against.

  One collision worth remembering: `files::Entry` and `git::Entry` both generate
  `Entry.ts`, and every exported type lands in one flat directory, so the second
  silently replaced the first. It surfaced as a type error here; with compatible
  shapes it would not have.

- **31. Shells beside the agent** — DONE. `Channel::Terminal` names a tmux
  target, `Shells`/`NewShell`/`KillShell` manage them, and the tab bar grew a
  `+`. 528 tests.

  Each shell is its own tmux session in the sandbox, which is what removes the
  contention rather than dropping `attach -d`: that flag evicts a client left
  behind by a crash and is worth keeping, and two tabs on one tmux session would
  have evicted each other instead. The same sandbox and the same policy -- a
  shell is not a way around the isolation, it is a second prompt inside it.

  What shells exist is asked of the sandbox. tmux already knows, and its answer
  outlives the window closing and a second window opening; a list kept in a
  client would show one closed from elsewhere and hide one opened there. The
  server names them too, since two windows adding at once would both pick
  `shell-2` and the second would silently attach to the first's.

  `tmux: None` on the channel means the agent's own, so a client written before
  any of this keeps working -- there is a test for exactly that, because it is
  the kind of compatibility that breaks silently.
- **31c. A window that pairs itself, and the Windows half** — DONE. Pairing from
  the header and from the empty screen, `sbx-core` building for Windows, and the
  `.msi`/NSIS bundles in the release. Pulled forward out of increment 35,
  because the alternative was a Windows user who cannot reach a server at all.

  The instruction the empty screen used to give -- run `sbxd pair` over there,
  `sbx connect` here, then reopen this window -- is fine on Linux and impossible
  on Windows, where there is no `sbx`: the CLI drives Docker, tmux and a
  gateway, none of which are on that side.

  The pairing is not a second implementation. `sbx_client::pair` is the parsing,
  the dial, the fingerprint check, the "is this an `sbxd` speaking this protocol"
  check and the save; `sbx connect` is that plus a `println!` and the dialog is
  that plus a form. Nothing is written until the server has answered, and what
  comes back is the server's own version -- the one thing a paste cannot fake.
  No error echoes the string back, since it carries a token.

  **The client half did not compile for Windows, and nothing said so.**
  `state.rs` reached for `std::os::unix::fs` to set 0600 on a key and 0700 on
  its directory, and `update.rs` for the executable bit, with no `cfg`
  anywhere -- so `sbx-core` failed to build for the one platform the desktop
  application was supposed to be a client from. Windows gets
  `%LOCALAPPDATA%\sbx`, chosen the way `$XDG_STATE_HOME` was: roaming is the
  half of a profile that follows a user to another machine, and a pairing token
  is a login to one host. There is no mode to set there and the module says so
  rather than pretending to enforce one. CI checks the target on every change,
  because what it guards against costs seconds here and a tagged release
  anywhere else.

  No Linux bundle, deliberately: a Tauri bundle links against the webkit2gtk of
  the distribution that built it, and one `.deb` would be a promise about GTK
  versions this cannot keep.

  **A third WebKit metrics bug, found by building the dialog.** The zero font
  metrics that gave the terminal a zero-height cell also make `line-height:
  normal` resolve to nothing on a form control, whose height is its line-height
  times its rows -- so every input and textarea in the window was a 14-pixel
  sliver with its text clipped through the middle. It survived because fields
  are typed into rather than read, and because a `<select>` beside them renders
  correctly, bringing its own metrics. A pairing string is the one field you
  read back, which is how it surfaced. Controls get the explicit line-height the
  rest of the document must not have, and their padding absorbs the two pixels
  it puts the baseline out by.
- **32. Worktree backend** — DONE. The `Backend` trait, the second
  implementation, and the labelling that keeps it honest. 552 tests, five of
  them against a real git repository.

  Everything in `ops`, `git`, `files`, `publish` and `seed` takes a backend
  now instead of a gateway client, and every function in them is unchanged
  otherwise. That is the shape of the whole increment: a worktree session is
  not a special case anywhere above the trait, it is a different set of answers
  to three questions -- where does an exec go, where are the files, is there
  any isolation to report.

  **The trait is all *where* and no *what*.** The scripts stay shared: one
  definition of the diff, the poll, the status scrape, the review paste and the
  seeder, each handed the paths and the tmux invocation it should use. A backend
  with its own copy of the diff script would be a second answer to what a diff
  is, which is the thing the two front ends already exist to avoid. `place` and
  `configure` are separate for the reason the record is written between them: a
  sandbox with no record yet is an orphan a refresh in another process will try
  to adopt, and imposing MCP endpoints made that window seconds wide.

  **The absence of isolation is stated, never implied.** `Isolation::None` is
  refused with a sentence rather than answered with an empty pane -- an empty
  policy pane is exactly what one that failed to load looks like, and the pane's
  whole job is to say what the session cannot reach. The wording is the
  server's, once, so the terminal and the window cannot drift; the protocol
  grew a `no-isolation` failure kind and the desktop's Tauri bridge grew from a
  `String` error into `{kind, message}`, which the comment on that type had
  predicted would happen the first time something needed to branch. `sbx ls`
  grew a `KIND` column, the tree grew a badge, and the facts pane trades
  `sandbox`/`policy` for `isolation`/`workdir`.

  Three things a worktree session cannot have, each of which had to be *said*:
  the policy pane, the events feed, and a publish with the same guarantee.
  `publish.rs` needed no code change for the last one -- the credential prelude
  already degrades to a plain `git` when the gateway's placeholder is absent --
  which is exactly why it needed a paragraph instead: the button is the same and
  the token is the server's.

  **The record cannot live in the working copy.** The invariant everything else
  rests on -- the sandbox is the source of truth about itself -- has no worktree
  equivalent. `.sbx/` in the working copy would be in every `git status` the
  agent runs, in every diff under review, and one `git clean -fdx` from gone. So
  it lives under the server's state directory, and adoption after a lost cache
  is that directory reconciled against the worktrees still on disk. Removing a
  session with no record answers `RecordOnly` rather than guessing at a
  directory: unlike a sandbox name, a worktree's path is not a function of the
  session's name once a root has been reconfigured.

  **tmux is the server's, and that is a naming problem, not a plumbing one.**
  Every sandbox has a tmux server to itself and can call the agent's session
  `agent`. Here they share one -- with each other, and with whatever the person
  at that machine is running -- so the agent is `sbx-<name>` and its shells are
  `sbx-<name>-shell-N`, and `shells` filters on the backend's prefix instead of
  "everything except the agent's". Without that, two sessions attach to one
  agent and your own tmux sessions are offered as a session's shells. There is
  an `#[ignore]`d test for exactly that, because it needs a tmux server.

  **One backend being unreachable is not the other's problem.** `refresh_with`
  used to be a gateway call that either worked or failed the command. A machine
  with no `openshell` on the path at all still has git, and refusing to list its
  worktree sessions would make the second backend useless precisely where it is
  most useful -- so a backend that cannot answer contributes a warning and its
  sessions pass through with the state they last had, never as `dead`. Only
  every backend failing is an error, because that is no information at all
  rather than a degraded list.

  Two bugs the live run found, both in the shared half rather than the new one.
  A `base=$(git symbolic-ref ...)` under the seeder's `set -e` **aborts the
  script and prints nothing at all**, which is how a create failed with `failed
  ` and an empty reason; the state file's last step is now reported when the log
  has nothing to quote, which is a better answer for every seeder failure of
  that shape. And `resolve_base_script` only ever tried `origin/<base>`, so a
  checkout with no remote had no base and the diff pane said so on every
  session -- it falls back to the local ref last, after the remote-tracking one,
  which is the order that keeps a sandboxed diff measured from where the work
  started. A worktree session records the checkout's current branch as its base
  for the same reason.

  What is not here: hook-driven status, since `sbx-status` is baked into the
  image and a worktree session's state comes from reading the screen; skills and
  MCP, since the agent is the server's own and packing them into the worktree
  would put them in `git status`; and a project made from a checkout with no
  origin, which `projects::add` still refuses because a project is also what a
  sandboxed session clones.

  Verified in the running application: a worktree session created from the
  window's form, badged in the tree, with the policy and events panes stating
  the absence and the facts pane naming the directory -- and the same session
  created, diffed, attached and removed from the command line against a
  repository with no remote at all.
- **33. Managed MCP and skill sync** — DONE. The catalog, the container
  lifecycle, the secret store, the client-to-server skill upload, and one screen
  over all three. 567 tests.

  Two documented procedures became two things the server owns. An MCP server was
  a `docker run` line copied out of `docs/mcp.md` -- with the credential on
  it -- and re-typed after every reboot; a skill was a path in the server's
  config file, which cannot reach the `~/.claude/skills` of the machine the
  window is on.

  **A catalog entry has a url or an image, never both.** A url is a server
  somebody else operates, exactly as before. An image makes it managed, and its
  url is *derived* -- `http://sbx-mcp-<name>:<port>/mcp` -- because the thing
  that names the container is the thing that joins it to the gateway's network.
  That deletes both ways a hand-written url goes wrong: a name no sandbox can
  resolve, and a `localhost` that means the sandbox itself. The keys that belong
  to a managed entry are refused beside a url rather than ignored, and an entry
  with both is refused outright: it would be a url pointing somewhere other than
  the container beside it, which nobody notices until an agent reports a dead
  tool.

  What a session records is unchanged. `Entry` carries the `Server` a session is
  given plus how to run it; the image, the environment and the secret names are
  the server's business and stay out of every session record.

  **Secrets go in and never come back out.** The store is
  `$XDG_STATE_HOME/sbx/secrets.json`, 0600, beside the pairing tokens and the
  TLS key; `secrets::get` is `pub(crate)`, so there is no path from a request
  handler to a value and the compiler is what says so rather than everyone
  remembering. The protocol carries names and whether each is set. `sbxd secret`
  reads the value from stdin because an argument lands in a shell history and in
  `ps`, and `start` passes it through the child's *environment* rather than as
  `--env NAME=value` for the same reason -- verified by inspecting the running
  container: the value was inside it and the argument list had only the name.

  **The states are the whole of the feature's honesty**, and one of them took
  measuring. `--restart unless-stopped` means Docker reports a container it is
  in the middle of restarting as `Running: true`, so an image crash-looping on a
  bad argument every two seconds read as healthy -- and did, on screen, until
  the restart count went into the inspect. `crashing` is now its own state with
  the container's own last output attached, which is the only thing that ever
  says why an image will not stay up. `detached` is the other: running, and not
  on `openshell-docker`, which is fine in `docker ps` and unreachable from every
  sandbox. Also measured: 29.7.2 says `error: no such object` where older
  versions said `Error response from daemon: No such object`, so matching the
  capital reported every never-started container as "docker could not be asked",
  which sends someone to look at their daemon.

  **Skills got a library, at `$XDG_DATA_HOME/sbx/skills`.** The client reads and
  packs its own `~/.claude/skills` on the Rust side of the bridge -- a webview
  cannot see a home directory -- with the same `payload` the seeder uses, and
  pushes them before every create as well as from the screen. That is what keeps
  the pointer-not-copy property across two machines: editing a skill on the
  laptop still means the next session gets the edit. Deliberately not the server
  user's own skills directory, which is theirs.

  The unpacking is where the care is, because a tar arriving from a client is a
  program's output rather than a promise: it goes into a staging directory and
  is checked before it is anywhere that matters -- exactly one top-level entry,
  named what the upload says it is named, with a `SKILL.md` in it -- and the
  name is refused by shape, since it decides a directory here and inside every
  sandbox. GNU tar skips `..` members itself, which is a good default and not a
  guarantee worth inheriting.

  One screen, and **every action on it answers with the whole view**, re-read.
  The same decision the git view made and for the same reason: these three
  explain each other, since a container that will not start is usually a secret
  that is not there. `sbx doctor`'s MCP check asks the same
  `mcp::statuses` the screen does, so a check that passes cannot disagree with a
  screen that says something is wrong, and `sbxd mcp`/`secrets`/`skills` give a
  headless server the same answers.

  **The generated bindings collided again, and silently this time.**
  `integrations::View` and `policy::View` are one flat directory apart, so the
  generated `Reply` carried `{ "reply": "integrations" } & View` pointing at the
  *policy* view -- a shape mismatch a webview would have found at runtime.
  Increment 31a hit the same thing with `files::Entry` and `git::Entry` and said
  it only surfaced because the shapes differed. Four renames later
  (`McpEntry`, `McpState`, `McpStatus`, `Integrations`), `gen-bindings.sh` now
  counts `ts(export)` attributes against files written and fails when they
  disagree, which was proved by re-introducing a collision and watching it fail.

  Verified against real Docker and a real window: two managed entries brought up
  by `sbxd` at startup, one reachable by container name from another container
  on the gateway's network with its secret in its environment, one crash-looping
  with its stack trace in the screen; `stop` taking the container away and the
  row going to `absent`; and this machine's own `ship-pr` pushed into the
  server's library from the window, listed by `sbxd skills` with the path it
  came from.
- **34. Task inbox** — DONE. GitHub, Azure DevOps and Jira read server-side,
  open-from-ticket, and the publish round trip. 580 tests.

  **REST for what the interface shows, MCP for what the agent gets**, and the
  split is the point rather than a duplication. A list on a timer rendered as
  rows, and a tool the agent calls when it decides to, are different consumers
  with different failure modes: a list that cannot be fetched is a pane with a
  message in it, a tool that cannot be reached is a session whose agent gives up
  on a step. One mechanism serving both would serve both badly.

  Read with `curl`, which is already on any machine that runs this and already
  how `publish.rs` talks to Azure DevOps from inside a sandbox; the alternative
  is an HTTP client, a TLS root store and a redirect policy pulled in for six
  requests. **The credential goes in on stdin**: `curl -K -` reads the url and
  the `Authorization` header from standard input, so a token is never in `ps`
  output or in the text of a failed spawn -- the same care the secret store took
  in 33, and every value interpolated into that config is quoted, because an
  unescaped quote there can start a line that names a file to write.

  Three trackers, one `Task`. Nine renderings or one shape, and the id is the
  tracker's own -- a work item id, an issue number, a Jira key -- because that
  is what a comment is addressed to. A GitHub task also carries its repository,
  since `/issues` spans several and a comment has to go to the right one. Each
  reader is split in two so the parsing is testable against captured answers,
  which is the only way to have any confidence in a reader of somebody else's
  JSON: a pull request coming back from the issues endpoint, a WIQL answer of
  ids with no second request to make, a Jira status nobody may rename for us.

  **A ticket names its session and its branch**, and both are decided on the
  server so the two front ends could not disagree. The key keeps its case in the
  branch, because a tracker's commit hooks and a reviewer both look for
  `PROJ-123`, and loses it in the session name, which has to satisfy
  `validate_name`. That needed `branch_prefix` in the config file and a
  `branch` on the request -- the first work branch that is not `sbx/<name>`
  since increment 1 -- so `session::validate_branch` refuses what git would
  before it reaches a shell in a sandbox and a remote.

  **A ticket does not know which repository it is about.** A Jira issue names a
  project and a work item names an area path; neither is a clone URL, and
  guessing from a name would be wrong exactly where it matters. So the row
  carries a project chooser, opening on the project of whatever is selected in
  the tree: the tracker says what to do and you say where.

  The round trip is the last thing a publish does, and both halves are
  best-effort: by then the branch is pushed and the pull request is open, so a
  tracker that cannot be written to costs a comment and not the publish. Jira is
  moved by *transition*, matched by name against what that issue can actually do
  from where it is -- a workflow only offers some of them -- and a name that is
  not among them comes back saying which are, because Jira's own answer names
  neither. Azure DevOps is a `System.State` patch with the json-patch content
  type. GitHub has no status to move to and says so rather than doing nothing.

  **The bug the loopback test existed to find**: curl reads a `-H` string with
  no colon in it as an instruction to *remove* a header, so building the
  credential as `Basic <token>` and passing it as a header sent the request with
  no `Authorization` at all. Every tracker would have answered 401 and every
  message would have blamed the token. Nothing in the unit tests could see it --
  the fix is one `format!` -- so there is now a test that sends a real request
  to a listener on `127.0.0.1` and reads the headers off the wire, which also
  covers the Atlassian Document Format a Jira comment has to be and the
  transition lookup.

  `sbx doctor` grew a check, because a tracker whose credential is not in the
  store produces an inbox **silently missing its rows**, which looks exactly
  like having nothing assigned to you. `sbx tasks` prints the same inbox the
  window shows, locally or through a server.

  Verified end to end against a tracker on loopback: three tickets read over the
  protocol, one started from the window, and the session's record carrying
  `tobias/INET-4821-order-backfill-throws-on-an-empty-batch` and the ticket it
  came from. Against a real Jira, Azure DevOps or GitHub it is unverified: that
  needs credentials in the server's store, which are the owner's to put there.
- **35. Ship it** — DONE. Notifications, usage and rate limits, and signing.
  Windows packaging and the install story for a server that is not local landed
  early, in increment 31c. 585 tests.

  **Claude Code hands out cost and rate limits in exactly one place: the status
  line.** No file, no endpoint -- a `statusLine` command it invokes on every
  render with a JSON payload on stdin. So the image bakes one in whose real job
  is to keep the payload where a poll can read it, exactly as `sbx-status` does
  for the hooks, and it prints the line the agent shows:
  `Opus 5 (1M context)  $0.07  5h 32%  ctx 2%`. The whole payload is kept and
  the reader takes what it recognises, because the shape belongs to Claude Code
  and grows.

  **Two things the documentation would have got wrong, and one it does not
  mention.** `resets_at` is epoch seconds, where the obvious reading of the
  changelog is an ISO instant -- a reader asking for a string got `None`, which
  looks like a window with no reset time rather than a parser that missed one.
  `rate_limits` is absent until the agent has actually called the API, so the
  first probe -- a session whose agent was never logged in -- had none, and that
  is the honest shape for a session sitting at a prompt. And beside the cost is
  a `context_window` with `used_percentage`: how full the context is, which is
  the number that says whether a session is about to compact, and the most
  useful thing in the payload. The test fixture is the captured payload rather
  than one written from the changelog.

  A rate-limit window is the **account's**, not the session's, so the two are
  displayed in different places: the windows in the header, the cost and the
  context on the session's own facts pane. Two sessions on one account report
  the same percentages, and showing them per session would be a lie about what
  is being measured.

  **The notification needed the window to know something it could not.** `Ls`
  reports what the *record* says -- `ready`, `idle`, `failed` -- and what the
  agent is doing is only ever in a poll, so a notification driven by the session
  list would never have fired. The window now opens a status channel per
  worktree rather than for the selected one, which is what the plan's list badge
  needed anyway: the tree shows the live agent state, and `waiting` finally
  appears in it.

  It fires on the *transition* into waiting -- a session sits in `waiting` until
  somebody answers it, and notifying on the state would notify every few seconds
  for as long as it waits -- and never for the first list after the window
  opens, because three sessions already waiting are three things that have been
  true for an hour.

  **The toast itself is unverified here, and that is the environment rather than
  the code.** WSLg has no `org.freedesktop.Notifications` on the session bus at
  all, so nothing can show one; what was verified is everything up to the call,
  including the `waiting` badge reaching the tree from a real sandbox. Windows,
  which is the platform this window ships to, has a notification service.

  Signing is conditional on a certificate being in the repository's secrets, and
  skipped when there is none so a fork can still cut a release. The thumbprint
  is read back from the imported certificate rather than configured, because a
  thumbprint in a config file and a certificate in a secret are two things to
  keep in step and only one of them is visible. Never run against a real
  certificate: there was none to test with, which the workflow says beside the
  step.

  Verified against a real agent in a real sandbox: the image rebuilt with the
  status line, a session created with a credential, one turn taken, and the
  payload read back through the poll, the stream and both front ends --
  `cost $0.07`, `context 2% of 1000k`, `5h 32%` and `7d 7%` in the header. The
  first attempt had no credential attached and the agent came up to `Not logged
  in`, which is how the "no rate limits until the API has answered" shape was
  measured rather than guessed.

## Risks

- **A listening daemon is a new attack surface**, and an authenticated one is
  root on its host. Mitigation: `127.0.0.1` by default, an explicit and noisy
  flag to widen, pinned certificates, hashed and revocable tokens -- and saying
  so plainly rather than implying more safety than there is.
- **Two backends dilute the pitch.** "Kernel-enforced isolation" and "also, a
  mode without any" is a harder sentence. Mitigation is the labelling in
  increment 29, and keeping sandboxed the default everywhere it is offered.
- **Tauri on Linux is WebKitGTK.** A heavy terminal there will be worse than on
  Windows. Accepted: the Linux user has the TUI, and the fallback if it does
  bite is serving the same web UI to a browser, which the transport already
  allows.
- **Version skew** between a shipped desktop app and a self-hosted `sbxd`.
  Mitigation: the unauthenticated `/version` and a client that refuses politely.
- **Scope.** This is several times the size of the TUI, and the TUI is the
  hedge: it keeps working the whole way through, so a stalled desktop app costs
  the new thing rather than the working one.
- **OpenShell 0.0.x churn now reaches a GUI too**, which is a slower thing to
  repair than a pane. Unchanged mitigation: all of it stays behind one trait.
