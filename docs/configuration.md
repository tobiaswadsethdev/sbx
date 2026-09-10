# Configuration

Every default `sbxd` takes is a flag, and `sbxd config --init` writes a file that
stops them being typed. `~/.config/sbx/config.toml`, beside the session cache,
all keys optional:

```toml
gateway    = "openshell"                              # unset: the active one
repo       = "https://github.com/octocat/Hello-World" # `sbxd new` with no --repo
base       = "develop"                                # unset: the remote's default
policy     = "feature-work"                           # a template, or a path to a YAML file
providers  = ["claude-oauth", "azure-pat"]            # credentials for a new session
repo_roots = ["~/dev", "~/work"]                      # where the picker looks
worktree_root = "~/.local/share/sbx/worktrees"        # where worktree sessions go
branch_prefix = "tobias"                              # <prefix>/<name> for a work branch
refresh    = "1s"                                     # unused since v0.4.0; still parsed
auto_update = true                                    # download new releases ahead of a restart

skills     = ["ship-pr"]                               # copied into every session

[[mcp]]                                               # one table per MCP server
name = "jira"                                         # see docs/mcp.md
url  = "http://mcp-atlassian:9000/mcp"                # ... a server you run

[[mcp]]
name    = "sentry"                                    # ... or one sbxd runs
image   = "ghcr.io/example/mcp-sentry:1.4"
port    = 9000
secrets = ["SENTRY_TOKEN"]                            # names; values live on the server

[[tracker]]                                           # the task inbox
kind    = "jira"                                      # see docs/inbox.md
site    = "https://your-org.atlassian.net"
email   = "you@example.com"
secret  = "JIRA_API_TOKEN"
```

Everything in it is a *default*: a flag on the command line wins, and so does an
explicit choice in the create form. `sbxd config` prints what is in force with
`*` for the file's answers and `-` for the built-in ones.

## Editing it from the window

**settings** in the desktop application's header writes the five keys that are
about what a new session starts with -- `branch_prefix`, `base`, `policy`,
`providers` and `auto_update`. The server's file, not the client's: a work
branch is named the same way whether the session was started from the window or
from `sbxd new`, and a window keeping its own prefix would be a second
convention that disagrees with the first. See [desktop.md](desktop.md#settings).

The rest of the file is not editable from there, and the omissions are the
point. `repo_roots`, `worktree_root` and `skills` are paths on the server;
`[[mcp]]` and `[[tracker]]` are lists of tables, each one a decision about what
an agent of yours can reach, and the integrations screen already says so about
the MCP half.

**The file is edited, not regenerated.** Each key is found and replaced where it
stands, so every comment `sbxd config --init` wrote is still there afterwards --
which matters because the comments are most of what the file is for. Clearing a
field removes the key rather than writing an empty one, because an absent key is
what gets the built-in default and `policy = ""` would say something the parser
does not mean. And the new text is parsed *before* it is written: every command
except `sbxd doctor` refuses to run against a config it cannot read, so a
settings screen that could save an invalid one would be able to break the server
from inside its own UI.

**A file that cannot be read stops the command**, rather than being quietly
replaced by the defaults -- a key that does nothing is indistinguishable from a
key that is not working, so a misspelled one is named back at you:

```
sbx: ~/.config/sbx/config.toml: TOML parse error at line 1, column 1
  |
1 | polciy = "feature-work"
  | ^^^^^^
unknown field `polciy`, expected one of `gateway`, `repo`, `base`, `policy`, ...
```

The one exception is `sbxd doctor`, which is the command you reach for when
something is wrong: it reports the error as a failed check and carries on with
the defaults. It also checks the `providers` you named still exist at the
gateway, since a stale name is the quietest failure here -- the form does not
tick it, the sandbox comes up without the credential, and the clone fails for
what looks like an authentication problem several steps later.

`refresh` is one number rather than six because the intervals underneath it are
measured and related to each other; it scales all of them, so `"4s"` polls a
quarter as often (41 execs in a 30 second window became 13) and `"500ms"` twice
as often. 250ms to 60s -- below that the terminal interface's 100ms input tick became the
limit and the extra `git status` inside every sandbox buys nothing. Nothing has
read it since that interface went in v0.4.0; the key is still parsed so a file
written before then still loads.

`auto_update` is on unless it is turned off. What it permits is a *download*:
`sbxd serve` checks every six hours and leaves a verified binary beside the
running one, and the swap happens at the next start rather than under a live
session -- [install.md](install.md#updating-without-being-asked) is the whole
of it. `false` for a machine that would rather not reach github at all, which
also turns the check off, not just the install.

Where a default meets something sbx already works out for itself, the more
specific answer wins:

* `providers` **replaces** the create form's guesswork, because an explicit list
  beats a heuristic and merging the two would attach a credential nobody asked
  for.
* `base` only fills a **detached HEAD**: the branch a checkout is sitting on is
  evidence about that repository, and a config entry is a guess about all of them.
* `repo` moves the picker's **cursor**, not its filter, so every other repository
  is still one keystroke away -- and typing drops the preference for good.
* `repo_roots` **replaces** the conventional places rather than adding to them,
  and `SBX_REPO_ROOTS` still wins over it.

`repo_roots` and `worktree_root` are both about the machine that *runs* the
sessions, which with a server is not the machine with the window on it: a
worktree is added to a checkout, and both the checkout and the worktree are the
server's. See [worktrees.md](worktrees.md).

Two things are deliberately *not* in this file. **Secrets** are named here and
stored in `$XDG_STATE_HOME/sbx/secrets.json`, because a config file is the kind
of thing people copy between machines and paste into an issue. And **uploaded
skills** are not listed at all: what a client has pushed into the server's
library is a directory listing rather than a decision, and a second list to keep
in step with it would only ever be wrong. See [mcp.md](mcp.md) and
[skills.md](skills.md).


---

[← Documentation](README.md) · [README](../README.md)
