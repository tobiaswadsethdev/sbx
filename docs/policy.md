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

In the policy pane, `w` widens egress to the package registries and `t`
tightens it back, without restarting the agent -- for the task that turns out
to need a dependency installed. Only the network section: the filesystem and
process sections are fixed when the sandbox is created, and the gateway will
accept a change to them, report it as effective, and never enforce it, so the
pane labels them and declines to offer it.

## Acting on a denial

`w` and `t` are one preset. The events feed is where the *specific* answer lives:
`j`/`k` move a cursor over the events, and `e` on the one you are looking at asks
what to do about the endpoint it names.

```
 endpoint  pastebin.com:443 for /usr/bin/curl  -- denied now
           a allow here · b block here  │  A allow always · B block always  │  esc cancel
```

Lowercase changes this session, through the same live `policy update` that `w`
uses; uppercase does that *and* records the endpoint in a global list applied to
every `sbx new` from then on. Nothing else on the keyboard responds while the
question is up -- `a` is attach everywhere else in the TUI, and answering a
question about egress must not also hand over the terminal. Any other key
cancels.

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
