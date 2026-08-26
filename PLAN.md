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
Increments 0-12 are done. What is left is the unscheduled list below.

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

Current state: increments 0-9 done, `main` at a clean tree, 234 tests, clippy
and rustfmt clean. `sbx doctor` should be all green; if the gateway is down,
`systemctl --user status openshell-gateway`.

The loop that works today, end to end:

```sh
sbx new --repo <url> --task "..." --policy feature-work \
        --provider claude-oauth --provider azure-pat
sbx            # or start one here: n, pick a repo, fill the form, enter
               # Enter to attach, Ctrl-b d to detach, q to quit
               # Tab cycles preview/diff/policy/events; w/t widen/tighten egress
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
  wedge the sandbox.** Both were assumed to be true the other way round -- the
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
