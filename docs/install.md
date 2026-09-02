# Installing sbx

## Prerequisites

Linux with systemd and a Docker daemon. Verified on Arch on WSL2; nothing here
is portable to macOS, because the isolation is kernel-enforced.

| | |
| --- | --- |
| [OpenShell](https://github.com/NVIDIA/OpenShell) | 0.0.110 -- CLI, gateway and sandbox helper |
| Docker | server 29.x, reachable by your user |
| tmux | on the host, for `sbx attach` |
| Rust | 1.89 or newer -- only to build `sbx` yourself (edition 2024, let-chains, `File::lock`) |

`sbx doctor` checks every one of them, plus the sandbox image and whether
systemd lingering is enabled, and says what to do about whatever is missing:

```
[  ok  ] version      sbx 0.2.0, newest
[  ok  ] openshell    openshell 0.0.110
[  ok  ] gateway      https://127.0.0.1:17670 0.0.110 (authenticated)
[  ok  ] docker       server 29.6.0
[  ok  ] tmux         tmux 3.6b
[  ok  ] linger       enabled
[  ok  ] image        sbx-base:latest built, claude 2.1.246
```

## Installing the pieces

**OpenShell.** OpenShell's own `install.sh` supports dpkg and rpm only; on
anything else install the release tarballs into `~/.local/bin`, which needs no
root. The gateway runs as a systemd *user* service:

```sh
systemctl --user enable --now openshell-gateway
openshell gateway add https://127.0.0.1:17670 --local --name openshell
openshell status                       # -> Connected, Authenticated (mTLS)
sudo loginctl enable-linger $USER      # WSL: or the gateway dies with your shell
```

The tarball names, checksums and the unit file are in
[manual-loop.md](manual-loop.md) and
[openshell-gateway.service](openshell-gateway.service).

**Providers.** One per credential the agents need. The profiles are in
`providers/`; `--credential KEY` reads the value from the environment at create
time and stores it in gateway state, so the shell that ran it can be closed:

```sh
openshell provider profile import --file providers/claude-code-oauth.yaml
read -rs -p "paste token: " CLAUDE_CODE_OAUTH_TOKEN   # `claude setup-token`
export CLAUDE_CODE_OAUTH_TOKEN
openshell provider create --name claude-oauth \
        --type claude-code-oauth --credential CLAUDE_CODE_OAUTH_TOKEN
```

`read` needs a TTY, so that has to be a real terminal. For Azure DevOps, do the
same with `providers/azure-devops-pat.yaml` (see [Git hosts](git-hosts.md)).

**`sbx` itself.** The policy templates and the whole image recipe -- Dockerfile,
status hook, Claude settings -- are compiled into the binary, so it needs
nothing from this tree at runtime except the provider profiles above, which the
`openshell` CLI reads directly. That is what makes a one-line install possible:

```sh
curl -fsSL https://raw.githubusercontent.com/tobiaswadsethdev/sbx/main/install.sh | sh
```

It works out which release fits this machine, downloads it, **checks it against
the release's published `SHA256SUMS` and installs nothing if that does not
match**, puts the binary in `~/.local/bin`, and finishes by running `sbx doctor`
so the prerequisites above are named rather than discovered one at a time. Read
it first if you would rather not pipe a script into a shell -- it is
[install.sh](../install.sh) in this repository, and downloading it and running
it separately works exactly the same.

Three things it takes, as flags or environment variables:

| | |
| --- | --- |
| `--bin-dir DIR` / `SBX_BIN_DIR` | where to install; default `~/.local/bin`, and it says so when that is not on your `PATH` |
| `--version vX.Y.Z` / `SBX_VERSION` | a specific release rather than the newest |
| `--from-source` / `SBX_FROM_SOURCE=1` | build with `cargo` instead of downloading |

Building it yourself is the other way, and the one to use from a checkout. It
is also the automatic fallback when no release is built for your architecture:

```sh
cargo install --path crates/sbx                                   # from a checkout
cargo install --git https://github.com/tobiaswadsethdev/sbx sbx --locked   # without one
```

Then:

```sh
sbx image build                      # also happens on first `sbx new`
sbx doctor
```

Start something: `sbx new --repo <url> --task "..."`, or `sbx` for the
terminal interface, where `n` does the same thing with a picker and a form --
[tui.md](tui.md).

There is a desktop workspace as well, and it talks to a server rather than to
the gateway directly -- so it works whether the sandboxes are on this machine or
another one. [server.md](server.md) is how to pair the two; [desktop.md](desktop.md)
is what the window does once they are.

If you would rather see each step yourself before trusting a tool with it,
[manual-loop.md](manual-loop.md) is the whole loop run by hand, against the
versions it was verified on.

## Updating

`sbx update` is the install script's three steps performed by the binary that
is already there: read the release list, verify the download against
`SHA256SUMS`, and replace itself.

```sh
sbx update                 # to the newest release
sbx update --check         # say what that would do, and do none of it
sbx update --tag v0.1.0    # to one named release, to get back to one that worked
sbx update --force         # reinstall the version already running
```

The replacement is a rename over the running binary, which Linux allows and
which means a torn download cannot leave half an `sbx` on your `PATH`. A
session already running is untouched -- its agent lives in a sandbox, not in
this binary -- but the sandbox image is versioned separately, so `sbx image
build` after an update is what picks up a change to the image recipe.

**Nothing updates itself.** There is no background check and no timer: `sbx
doctor` reports when a newer release is out, and that is the whole of it.

```
[ warn ] version      sbx 0.2.0; 0.3.0 is out
         fix: sbx update
```

A binary installed with `cargo install` can still be updated this way, since it
is replaced where it stands. Going the other way -- back to a build from the
tree -- is `cargo install --path crates/sbx` again.

---

[← Documentation](README.md) · [README](../README.md)
