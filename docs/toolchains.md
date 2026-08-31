# Toolchains

A sandbox that can only clone and read is a sandbox that can only write code
nobody has compiled. The base image carries node and python, because the
community image does. Anything else -- the .NET SDK, a Rust toolchain -- is asked
for per session:

```sh
sbx new --repo <url> --task "fix the failing test" --toolchain dotnet
sbx new --repo <url> --task "..."                  --toolchain dotnet,rust
sbx toolchains                                     # what is available
```

In the TUI it is a field on the create form, beside the policy, and it usually
arrives filled in: a checkout with a `Cargo.toml` in it comes up with `rust`
ticked, one with a `.csproj` a level down comes up with `dotnet`. `space`
toggles, and an answer you change by hand stays changed.

| Toolchain | What it installs | What it may reach |
| --- | --- | --- |
| `dotnet` | the .NET SDK, current LTS channel, in `/usr/local/dotnet` | `api.nuget.org`, read-only |
| `rust` | rustc, cargo, rustfmt and clippy, in `/usr/local/rust` | `index.crates.io` and `static.crates.io`, read-only |
| `node` | nothing -- the base image already has it | `registry.npmjs.org`, read-only |

## Why the image, and not the sandbox

The agent cannot install a toolchain, and this is deliberate on both counts:
`/usr/local` is not writable by the sandbox user, and no policy template lets a
sandbox reach a download host. Widening the policy far enough for `dotnet-install.sh`
to work would hand every session a route to arbitrary tarballs on the internet,
which is most of what the isolation is for.

So the toolchain is the image's business, exactly as the agent's own version is.
It is resolved from the publisher's release manifest at build time, checked
against the checksum published beside it, and the build fails rather than
shipping something that did not verify.

## One image per set of toolchains

Each set is its own tag, layered onto the base image:

```
sbx-base:latest        the base -- what a session with no toolchain runs
sbx-base:dotnet        the base plus the .NET SDK
sbx-base:dotnet-rust   the base plus both
```

Docker shares the base's layers between all of them, so a variant costs its own
toolchains and not another copy of the five gigabytes underneath. A Rust session
never carries the .NET SDK, and its policy never mentions nuget.

The tag is a pure function of the *set*, so `--toolchain rust --toolchain dotnet`
and `--toolchain dotnet,rust` name one image rather than building two identical
ones. It is built on first use, or ahead of time:

```sh
sbx image build --toolchain dotnet,rust
```

The TUI will not build one -- the build streams docker's output, which would tear
the interface apart mid-frame -- so a create asking for a toolchain nobody has
built yet fails with the command that builds it.

A variant is `FROM sbx-base:latest`, which means rebuilding the base for a newer
agent leaves the variants behind on the old one. Nothing about that looks wrong
from outside: sessions start, the toolchain works, and the agent is whatever
version it was. `sbx doctor` is what says so:

```
[  ok  ] image        sbx-base:latest built, claude 2.1.246
[ warn ] toolchains   sbx-base:dotnet older than sbx-base:latest, so still on its previous agent
         → sbx image build --toolchain dotnet
```

When they are current it reports what each one actually carries, read from a
manifest the layers write inside the image rather than inferred from the tag:

```
[  ok  ] toolchains   sbx-base:dotnet (dotnet 9.0.317); sbx-base:rust (rust 1.98.0)
```

## The registry, and the binary that may reach it

A toolchain is not only an install. `cargo build` on anything with a dependency
needs crates.io, and that endpoint is opened for the session that asked for the
toolchain and for no other -- bound to cargo, and to nothing else in the sandbox:

```
  crates-io    index.crates.io:443    read-only
               /usr/local/rust/bin/cargo
```

`net-open.yaml` argues the other half of this, and named crates.io as exactly
what not to ship:

> crates.io is not here [...] the sandbox image ships no Rust toolchain, so the
> endpoint would be unreachable decoration. Add it alongside a cargo binary if
> the image ever grows one.

This is where it arrives, alongside a cargo that can reach it. Which also means
`--toolchain` is a *policy* choice as much as an image one, and the policy pane
shows it like any other rule.

**`read-only`, not `full`.** A registry fetch is thousands of unpredictable
paths, so an allow-list would either be wrong or be `/**`; "GET anything here,
write nothing" is what is meant, and publishing a package is not something a
sandboxed agent should be able to do by accident.

**The binary is the resolved one.** The gateway matches `/proc/<pid>/exe`, so the
convenience symlink in `/usr/local/bin` is invisible to it: dotnet's rule names
`/usr/local/dotnet/dotnet`, and npm's names `/usr/bin/node` rather than
`/usr/bin/npm`, which is a JavaScript file behind a `#!/usr/bin/env node` line.
Getting one wrong produces a denial naming a path the policy appears to contain,
so each one is checked against the layer that installs it by a test.

## What the layers put where

`/usr/local` is read-only to the agent, which is the point -- it cannot replace
its own compiler. Everything a build *writes* goes under `/sandbox`, the writable
half:

| | |
| --- | --- |
| `CARGO_HOME` | `/sandbox/.cargo` -- the registry index, the crate cache, `cargo install` output |
| `NUGET_PACKAGES` | `/sandbox/.nuget/packages` |
| `DOTNET_CLI_HOME` | `/sandbox/.dotnet` |

and the three the SDK is quietened with: `DOTNET_CLI_TELEMETRY_OPTOUT`,
`DOTNET_NOLOGO` and `NUGET_CERT_REVOCATION_MODE=offline`.

These are set twice, for the reason the base image sets its locale twice: `ENV`
covers a shell someone opens by hand, and `set-environment` in `/etc/tmux.conf`
is what actually reaches the agent, whose environment comes from the tmux server
the seeder starts. The gateway does not pass an image's environment through to an
exec.

The .NET SDK's telemetry and its first-run workload banner are both turned off,
for the reason the image already turns off Claude Code's auto-updater: the
sandbox denies the traffic behind them, and a denial with nothing worth
investigating behind it is noise in the events pane.

So is NuGet's *online* certificate-revocation check, and that one was measured
rather than predicted. A `dotnet add package Newtonsoft.Json` succeeded and left
six denials in the feed, all of them NuGet checking the signing certificates
against `www.microsoft.com/pkiops/crl/...`, `crl3.digicert.com` and
`ocsp.digicert.com`. The restore does not need them -- the check is soft-fail,
which is why it worked -- so the choice was between allowing three more hosts and
not making the request. `NUGET_CERT_REVOCATION_MODE=offline` does the latter, and
leaves the same restore with a feed of ten allows and nothing else. Signature
verification itself is untouched.

## Adding one

`crates/sbx/src/toolchain.rs` is one table, and a toolchain is an entry in it
plus a Dockerfile fragment under `images/sbx-base/toolchains/`. The entry names
the registries and the *kernel-resolved* binaries that may reach them, and the
markers that make the create form tick it. The tests in that module check the
three halves against each other: every binary in a rule is a path its layer
installs, every layer records itself in the manifest `doctor` reads, and no
toolchain ships without a registry it could fetch from.

---

[← Documentation](README.md) · [README](../README.md)
