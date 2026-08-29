# Installing sbx

## Prerequisites

Linux with systemd and a Docker daemon. Verified on Arch on WSL2; nothing here
is portable to macOS, because the isolation is kernel-enforced.

| | |
| --- | --- |
| Rust | 1.89 or newer (edition 2024, let-chains, `File::lock`) |
| [OpenShell](https://github.com/NVIDIA/OpenShell) | 0.0.110 -- CLI, gateway and sandbox helper |
| Docker | server 29.x, reachable by your user |
| tmux | on the host, for `sbx attach` |

`sbx doctor` checks every one of them, plus the sandbox image and whether
systemd lingering is enabled, and says what to do about whatever is missing:

```
[  ok  ] openshell    openshell 0.0.110
[  ok  ] gateway      https://127.0.0.1:17670 0.0.110 (authenticated)
[  ok  ] docker       server 29.6.0
[  ok  ] tmux         tmux 3.6b
[  ok  ] linger       enabled
[  ok  ] image        sbx-base:latest built, claude 2.1.246
```

## Installing the pieces

**OpenShell.** The official `install.sh` supports dpkg and rpm only; on
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
`openshell` CLI reads directly:

```sh
cargo install --path crates/sbx      # -> ~/.cargo/bin/sbx
sbx image build                      # also happens on first `sbx new`
sbx doctor
```


Then start something: `sbx new --repo <url> --task "..."`, or `sbx` for the
TUI, where `n` does the same thing with a picker and a form --
[tui.md](tui.md).

If you would rather see each step yourself before trusting a tool with it,
[manual-loop.md](manual-loop.md) is the whole loop run by hand, against the
versions it was verified on.

---

[← Documentation](README.md) · [README](../README.md)
