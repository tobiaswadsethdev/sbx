# Increment 0: the loop, driven by hand

Verified 2026-08-21 against OpenShell 0.0.110, Docker driver, Arch Linux on WSL2.
Every command below was actually run. This is the contract the Rust client in
Increment 1 is built against.

## Install (Arch / any non-dpkg, non-rpm distro)

The official `install.sh` **fails on Arch**: it only supports dpkg or rpm.
Install from the release tarballs instead — no root required.

```sh
V=0.0.110
B=https://github.com/NVIDIA/OpenShell/releases/download/v$V
for f in openshell-x86_64-unknown-linux-musl \
         openshell-gateway-x86_64-unknown-linux-gnu \
         openshell-sandbox-x86_64-unknown-linux-gnu; do
  curl -fLsS -O $B/$f.tar.gz
done
# verify against openshell{,-gateway,-sandbox}-checksums-sha256.txt, then:
for f in *.tar.gz; do tar xzf $f; done
install -m755 openshell openshell-gateway openshell-sandbox ~/.local/bin/
```

The gateway runs as a **systemd user service**, unit copied from the official
`.deb` and repointed at `~/.local/bin`: see `docs/openshell-gateway.service`.

```sh
systemctl --user daemon-reload
systemctl --user enable --now openshell-gateway
openshell gateway add https://127.0.0.1:17670 --local --name openshell
openshell status     # -> Connected, Authenticated (mTLS transport)
```

WSL note: `sudo loginctl enable-linger $USER`, otherwise the gateway (and every
running sandbox) dies when the last shell exits.

## Create a session sandbox

```sh
openshell sandbox create \
  --name sbx-demo \
  --label sbx.session=demo \
  --policy policies/feature-work.yaml \
  --no-auto-providers --no-tty \
  -- sh -c 'echo sandbox-ready'
```

**~1 second to Ready** with the base image cached. Cold pull is the only slow
path, so a prebaked image plus a warm pool makes session creation feel instant.

Base image (`ghcr.io/nvidia/openshell-community/sandboxes/base:latest`) ships:
git 2.43, node 22, python 3.13, uv, curl, gh 2.92, ssh, **claude 2.1.143**.
It does **not** ship tmux, jq, vim, or rg. Runs as uid 998 `sandbox`, HOME=/sandbox.

## What the default policy actually allows

Measured from inside, not read from docs:

| Probe | Result |
| --- | --- |
| `curl https://{github,pypi,example}.com` | **denied** (000) |
| `getent hosts github.com` | **denied** (no DNS) |
| write `/sandbox`, `/tmp` | allowed |
| write `/etc`, `/usr` | denied |

Deny-all egress by default, including DNS.

## Policy is per-binary, not just per-host

The schema lives in `NVIDIA/OpenShell-Community:sandboxes/base/policy.yaml`.
Each `network_policies` entry binds endpoints to specific **binaries**
(identity resolved via `/proc/net/tcp` inode -> `/proc/{pid}/exe`, with parent
walking), and can gate HTTP method + path. Our `policies/feature-work.yaml`
enables `git-receive-pack` (push), which the community base policy comments out.

Demonstrated with that policy applied:

```
git clone https://github.com/octocat/Hello-World.git   -> SUCCEEDS
curl https://github.com                                 -> DENIED (000)
curl https://example.com                                -> DENIED (000)
```

Same host, different binary, different answer. This is the capability
claude-squad structurally cannot offer, and it should drive the product's
policy pane.

## Attach: host tmux -> sandbox connect

The v0 attach model, verified working:

```sh
tmux new-session -d -s sbx-demo -x 200 -y 50 "openshell sandbox connect sbx-demo"
tmux capture-pane -p -t sbx-demo      # -> preview pane content + status scraping
tmux send-keys    -t sbx-demo 'id' Enter   # -> inject the initial task prompt
```

Takes ~10s for the SSH session to come up. `openshell sandbox ssh-config <name>`
also emits a `ProxyCommand`-based Host entry, so plain `ssh openshell-<name>.default`
works — that is the hook for the `--editor vscode` path later.

**Caveat:** the base image has no tmux, so the agent process is a child of the
SSH session. Host tmux detach keeps it alive (the pane holds the connection),
but a dropped connection kills the agent. Increment 1 should bake tmux into a
custom image via `--from` so the agent survives inside the sandbox instead.

## Agent authentication (resolved)

The builtin `claude-code` provider profile takes an **API key** only
(`ANTHROPIC_API_KEY` / `CLAUDE_API_KEY`), which bills per token. For
subscription auth, `claude setup-token` mints a long-lived OAuth token that
Claude Code reads from `CLAUDE_CODE_OAUTH_TOKEN`. That is carried by a custom
provider profile, `providers/claude-code-oauth.yaml`:

```sh
openshell provider profile import --file providers/claude-code-oauth.yaml
# in a real terminal -- `read` needs a TTY, so this cannot run through a
# non-interactive shell:
read -rs -p "paste token: " CLAUDE_CODE_OAUTH_TOKEN && export CLAUDE_CODE_OAUTH_TOKEN
openshell provider create --name claude-oauth \
  --type claude-code-oauth --credential CLAUDE_CODE_OAUTH_TOKEN
```

`--credential KEY` (no `=VALUE`) reads the value from the environment **at
create time** and stores it in gateway state. It is not a reference to the
variable: verified by creating a sandbox from a shell where
`CLAUDE_CODE_OAUTH_TOKEN` was unset and finding it injected correctly. The
shell that ran `provider create` can be closed.

Updating a custom profile in place needs the live `resource_version`, so export
first, then update:

```sh
openshell provider profile export claude-code-oauth   # read resource_version
openshell provider profile update --file <edited>.yaml claude-code-oauth
```

The repo keeps the profile in import form (no `resource_version`), since that
is what a fresh gateway needs.

### Credentialed endpoints must be L7-inspectable

Attaching the provider initially failed:

```
credentialed endpoint 'statsig.anthropic.com:443' in rule 'claude_code'
uses L4-only; configure L7 inspection or explicitly set
allow_uninspected_credentials: true
```

The gateway will not inject a credential into a connection it can only see at
L4. Any host claimed by a credentialed provider must either terminate TLS
(`protocol: rest, tls: terminate`) or opt out explicitly. Rather than inspect
telemetry, `statsig.anthropic.com` and `sentry.io` were dropped from both the
policy and the profile -- denying agent telemetry is a feature here.

### Verified end to end

With `policies/feature-work.yaml` and `--provider claude-oauth`:

| From inside the sandbox | Result |
| --- | --- |
| `claude -p ...` -> api.anthropic.com | **authenticates** |
| `curl` -> api.anthropic.com | denied |
| `curl` -> example.com | denied |
| `git clone` -> github.com | allowed |

The token is injected as an environment variable and is **not** written to the
sandbox filesystem (no `/sandbox/.claude/.credentials.json`). The agent can
reach Anthropic; nothing else in the sandbox can, so there is no trivial path
to exfiltrate the credential it was given.

## Gateway constraints worth knowing (measured on 0.0.110)

None of these are in the docs; each was found by hitting it.

| Constraint | Value |
| --- | --- |
| Sandbox name length | **19 characters max** |
| Label value length | 63 characters max |
| Label value charset | `[A-Za-z0-9._-]` only -- no `/`, so branch names and repo URLs cannot be labels |
| Sandbox phases | `Provisioning`, `Starting`, `Ready`, `Stopping`, `Stopped`, `Deleting`, `Error`, `Unknown` |

The 19-character sandbox-name cap is the tightest constraint in the system. With
an `sbx-` prefix it leaves 15 characters for a session name, which is why names
are slugified by dropping whole trailing words rather than truncating.

The phase list came from `SANDBOX_PHASE_*` strings in the gateway binary. It
matters that `Deleting` is in it: deletion is asynchronous, so a removed sandbox
stays listed in `Deleting` for a while, and treating that as alive makes a
deleted session keep reporting whatever state it last had.

## Session model

The **sandbox is the source of truth**, not the local cache:

* `/sandbox/.sbx/meta.json` inside each sandbox holds the full session record,
  rewritten on every state change
* labels (`sbx.managed=true`, `sbx.session=<name>`) carry identity only, since
  label values cannot hold a URL or a branch
* `~/.config/sbx/sessions.json` is a cache and can be deleted at any time

Verified: deleting the cache and running `sbx ls` re-adopts every live session
by reading the record back out of each sandbox.

## Running the agent inside the sandbox

The agent runs under a tmux session **inside** the sandbox (`agent`), not in a
tmux session on the host. That was a change from the original plan, and it is
better on every axis: the agent survives losing its connection, its output can
be scraped with `capture-pane` without anything host-side, and it removes a
whole layer. Attach is then just:

```sh
openshell sandbox exec -n <sandbox> --tty -- \
  sh -c 'tmux -f /etc/tmux.conf attach -d -t agent'
```

`openshell sandbox connect` cannot be used for this: it takes no remote command.

Three things had to be solved to make it work.

### Landlock blocks pseudo-terminals by default

tmux dies with `create window failed: fork failed: Permission denied` and
`openpty` reports `out of pty devices`, even though `/dev/pts/ptmx` is mode
`crw-rw-rw-`. fork, setsid and the devpts mount are all fine -- Landlock simply
governs `/dev` too, and the default policy grants only `/dev/null`.

Add **`/dev/pts`** to `filesystem_policy.read_write`. Do **not** also add
`/dev/ptmx`: it is a symlink to `pts/ptmx`, granting the directory already
covers the real device, and adding the symlink makes the supervisor crash-loop
at startup with nothing useful in the logs.

### Killing an attach abruptly wedges exec for that sandbox

If the `exec --tty` process is killed rather than detached from, every
subsequent `exec` against that sandbox hangs forever, including `echo hi`. The
sandbox still reports `Ready`, the tmux server and the agent keep running, and
an orphaned `tmux: client` is left behind. Other sandboxes are unaffected, so
the blast radius is one session.

A clean detach (`Ctrl-b d`) never triggers it: the exec exits 0 and the next
exec works immediately. So `sbx` never kills the attach child -- it waits for
the user to detach -- and attaches with `-d` so a client stranded by an earlier
crash is evicted rather than shared.

### Claude Code's first-run onboarding

A fresh sandbox is a fresh `HOME`, so the agent opens on the theme picker and
then the trust prompt and never reaches its task. The image pre-seeds
`/sandbox/.claude.json` with `hasCompletedOnboarding`, the baked-in
`lastOnboardingVersion`, and `hasTrustDialogAccepted` for `/sandbox/repo`.
