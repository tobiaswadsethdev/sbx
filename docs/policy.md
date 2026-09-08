# Policy

The isolation is the point, so it is visible rather than buried in a YAML file.
The **policy** pane shows the rules the gateway is actually enforcing, per
binary, and the **events** pane is the allow/deny feed behind them:

```
┌ events (UTC) - add-tests ───────────────────────────────────────────────────┐
│  11:15:02  allow  GET github.com:443/octocat/Hello-World.git/info/refs [git]│
│▌ 11:15:02  DENY   /usr/bin/curl(93) -> pastebin.com:443                     │
│▌           endpoint pastebin.com:443 is not allowed by any policy           │
└─────────────────────────────────────────────────────────────────────────────┘
```

The events feed is **kept on disk**, one file per session under
`~/.config/sbx/events/`, because the gateway's log is a rolling window and sbx is
what makes it roll: every exec it takes to read a sandbox writes three lines of
its own, so at these poll intervals a 1500-line window covers about two minutes
and held *one* event worth showing. Each fetch is merged into what the session has
already shown, deduplicated and trimmed to the last few thousand, so the feed is a
record rather than a peephole -- and closing the tool no longer looks like it wiped
the log. Destroying a session takes its history with it.

`sbxd policy <name> --widen` opens egress to the package registries and
`--tighten` closes it back, without restarting the agent -- for the task that
turns out to need a dependency installed.

Only the network section. The filesystem and process sections are fixed when
the sandbox is created, and the gateway will accept a change to them, report it
as effective, and never enforce it -- so nothing here offers one, and the policy
view labels those sections rather than pretending they are live.

The preset offers npm and PyPI and not crates.io or nuget, which is not an oversight:
it grants them to `/usr/bin/node` and `/usr/local/bin/uv`, and those are in every
sandbox because the base image has them. A rule for cargo in a sandbox with no
cargo in it would be decoration -- the same argument `net-open.yaml` makes. A
toolchain is what brings both halves: `sbxd new --toolchain rust` runs the session
on an image carrying cargo *and* opens crates.io for it, so a rule like

```
network - allow_index_crates_io_443
  binaries    /usr/local/rust/bin/cargo
  endpoint    index.crates.io:443  rest  enforce  read-only
```

in the pane came from `--toolchain`, granted at create to that binary and to
nothing else in the sandbox. See [toolchains.md](toolchains.md).

## Acting on a denial

`--widen` and `--tighten` are one preset, all or nothing. The events feed is
where the *specific* answer lives -- `sbxd events <name>` names the endpoint and
the binary of each denial, and the global lists are what turn one into a
standing rule.

A session-level change goes through the same live `policy update` that
`--widen` uses. Recording the endpoint in a global list applies it to every
`sbxd new` from then on:

```sh
sbxd endpoints                                     # what is on them
sbxd endpoints --allow crates.io:443 --binary /usr/bin/cargo
sbxd endpoints --block pastebin.com:443
```

That writes the same file under the same lock, and applies to sandboxes started
from then on rather than to one already running.

An allow binds the endpoint to **the binary the event named**, not to the
sandbox: allowing `github.com:443` off a denied `curl` grants it to curl and
leaves git's own rule alone. That is also why an event decided by an L7 rule --
`GET httpbin.org:443/ip`, which names a method and a path and no binary -- can be
blocked but not allowed: an endpoint rule with no binaries grants nothing, and
issuing one would report a change that did nothing.

**A block is a removal, not a veto.** OpenShell denies by default and has no
deny-that-outranks-an-allow at L4, so blocking `pastebin.com` is a no-op -- it was
never reachable -- and blocking `platform.claude.com` is real, because
`feature-work.yaml` grants it. The pane says which, per entry:

```
── global lists - applied to every new session
  allow       pastebin.com:443  NOT in this policy
              /usr/bin/curl
  block       platform.claude.com:443  STILL in this policy
  block       nowhere.example.com:443  gone from this policy
```

The third column is the point: a list entry describes what a *new* session gets,
and the session in front of you may predate it or have moved since. The lists
live in `~/.config/sbx/endpoints.json`, are written under a lock like the session
cache, and are applied to a fresh sandbox in one `policy update` before the clone
starts -- so nothing has run in it yet. A block that fails to apply **fails the
create**; an allow that fails is a warning. The two are not symmetric: a missing
allow announces itself the moment the agent tries, and a missing block never
mentions itself again.

There is no key for taking an entry off a list -- `A` and `B` move an endpoint
between them, and removing it outright means editing the file, which is plain
JSON and hand-editable.


---

[← Documentation](README.md) · [README](../README.md)
