# Installing sbx

There are two things to install and they do not go in the same place. **`sbx`
and `sbxd` run where the sandboxes are**, which is Linux, because the isolation
is kernel-enforced. **The desktop application runs where you are sitting**,
which may be the same machine or may be Windows -- it makes requests of an
`sbxd` and needs no gateway, no Docker and no tmux of its own.

Most of this page is the first half. [The desktop
application](#the-desktop-application) at the end is the second, and is all that
a Windows machine needs.

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
another one. It is the next section; [desktop.md](desktop.md) is what the window
does once it is running.

If you would rather see each step yourself before trusting a tool with it,
[manual-loop.md](manual-loop.md) is the whole loop run by hand, against the
versions it was verified on.

## The desktop application

A window onto an `sbxd`. It holds no sandboxes and starts none itself: it dials
a server, pins that server's certificate, and asks. So the machine it runs on
needs none of the prerequisites above -- and the server it dials can be this
machine, a box on the LAN, or the Linux side of the same laptop.

Whichever platform, the last step is the same: **the window pairs with a server
from its own dialog**, so nothing above has to be installed beside it. Run
`sbxd pair desktop --host <the address the window will dial>` on the server,
paste the `sbx://…` line it prints into the window, and that is the install
finished. [desktop.md](desktop.md#connecting-it-to-a-server) is that step in
full, and [server.md](server.md) is the case where the two are on different
machines.

### Linux

Built from the tree. A Tauri bundle links against the webkit2gtk of the
distribution that built it, so a `.deb` or an AppImage published here would be a
promise about GTK versions it could not keep -- which is why the release page
carries a Windows installer and no Linux one.

The libraries, with their development headers:

| | |
| --- | --- |
| Arch | `sudo pacman -S webkit2gtk-4.1 gtk3 libsoup3 base-devel curl file openssl librsvg` |
| Debian, Ubuntu | `sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev build-essential curl file libssl-dev librsvg2-dev` |
| Fedora | `sudo dnf install webkit2gtk4.1-devel gtk3-devel libsoup3-devel openssl-devel curl file librsvg2-devel` |

Tauri's own [prerequisites](https://v2.tauri.app/start/prerequisites/) page is
the list that is kept current; these are the three that matter -- `webkit2gtk-4.1`
is the engine, and the two bugs in [desktop.md](desktop.md#the-font-metrics-webkit-gets-wrong)
are its. Node 22 or newer and the same Rust as the rest of the tree are the
other two.

```sh
cd apps/desktop
npm install
npm run tauri build      # bundle in src-tauri/target/release/bundle/
npm run tauri dev        # or run it from the tree, which is what to do while working on it
```

**`npm run tauri dev` rather than the debug binary.** A development build loads
the frontend from Vite's dev server, and that is what starts it; running
`src-tauri/target/debug/sbx-desktop` on its own gives a window that says
`Operation was cancelled` and reads exactly like a broken frontend.

### Windows

There is no `sbx` for Windows and there is not meant to be. The CLI drives
Docker, tmux and a gateway, and none of those are on that side; what runs there
is the window, which pairs itself. This is the arrangement the server was built
for: Linux in WSL doing the work, the window out on Windows.

Download the installer for the release you want from the [releases
page](https://github.com/tobiaswadsethdev/sbx/releases) -- either of:

```
sbx-desktop-vX.Y.Z-x86_64-pc-windows-msvc.msi          # Windows Installer
sbx-desktop-vX.Y.Z-x86_64-pc-windows-msvc-setup.exe    # the same application, NSIS
```

Both are covered by the release's `SHA256SUMS`, the same file `install.sh`
verifies a Linux binary against, so an installer can be checked before it is
run:

```powershell
(Get-FileHash .\sbx-desktop-vX.Y.Z-x86_64-pc-windows-msvc.msi -Algorithm SHA256).Hash.ToLower()
```

**A release built without a signing certificate is unsigned**, and SmartScreen
says so with a full-width warning before it will run one. The checksum above is
the integrity story either way -- it is the one that does not expire -- and the
release workflow signs the installers when a certificate is in the repository's
secrets (`WINDOWS_CERTIFICATE`, a base64 PFX, and
`WINDOWS_CERTIFICATE_PASSWORD`), skipping it when there is none so that a fork
can still cut a release.

WebView2 is the only runtime it needs, and Windows 11 ships with it; on Windows
10 the installer's own prompt or Microsoft's Evergreen bootstrapper supplies it.

Building it there instead needs Rust, Node 22 or newer, and the MSVC build tools
(the *Desktop development with C++* workload), then the same two commands as on
Linux. Only the client half of this repository compiles for Windows, which CI
checks on every change; `sbx` and `sbxd` do not, and are not asked to.

**Updating is downloading the newer installer.** `sbx update` replaces a Linux
binary in place and has no Windows half; a `.msi` installed over an older one
upgrades it. There is no background check on either platform.

**If the server is in WSL**, which is the case this was built for, the address
the window dials depends on how WSL is networked -- mirrored means
`localhost:17671` and NAT means an address that changes on every restart. `sbx
doctor` on the Linux side says which is in force and what to dial. See [the WSL
case](server.md#the-wsl-case).

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
