# The sandbox image, and the agent inside it

Each agent runs under a tmux session *inside* its own sandbox, so it keeps
working whether or not anything is attached to it.

The image bakes a `settings.json` for it, because a fresh sandbox has a fresh
`HOME` and an agent that has to be configured on arrival is an agent that stops
to ask:

| | |
| --- | --- |
| `model` | `opus[1m]` -- an alias, so it follows the newest Opus and keeps the million-token context |
| `permissions.defaultMode` | `auto`, so the agent handles its own permission prompts |
| `attribution` | `commit` and `pr` both empty, so nothing is stamped -- an empty string is what silences it, where an absent key means the default trailer |
| `copyOnSelect` | off. Not a `settings.json` key -- it lives in the global `.claude.json` the image also writes, and defaults to on; selecting text to read it should not take the clipboard of a terminal you are borrowing |
| `env` | the auto-updater, non-essential traffic and the plugin marketplace, all off |
| `hooks` | the status reporter, so the state column has something to read |

**Auto mode** is Claude Code's own middle setting: it judges each tool call and
executes what it considers safe, rather than stopping for every edit
(`acceptEdits` stops for everything that is not one) or not asking at all
(`bypassPermissions`). Claude Code's own advice is to use it "only in isolated
environments", which is the one thing sbx can actually promise -- and it is the
whole reason to run several agents at once, since an agent that stops on the
first edit is an agent you are still babysitting. `Shift+Tab` inside a session
changes it, and `/model` changes the model, for that session.

The three environment variables are all there because the sandbox *denies* the
traffic behind them, and a denial with nothing worth investigating behind it is
noise in the events pane. With them set, a session that clones, edits and answers
produces a feed with no denials in it at all.

`sbx image build` installs the newest Claude Code release rather than whatever
the community base image happens to have frozen -- it shipped 2.1.143 while
2.1.246 was current, and an agent cannot upgrade itself from inside a sandbox
with no writable install path and no route to the download service. The version
is resolved on the host and passed in as a build arg, so a rebuild really does
fetch what is newest instead of being answered from a cached layer, and the
download is checked against the release manifest's SHA-256.
`--build-arg CLAUDE_VERSION=2.1.246` pins a specific one. `sbx doctor` reports
what the built image carries and warns when a newer release is out.

---

[← Documentation](README.md) · [README](../README.md)
