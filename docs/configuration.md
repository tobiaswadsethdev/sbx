# Configuration

Every default `sbx` takes is a flag, and `sbx config --init` writes a file that
stops them being typed. `~/.config/sbx/config.toml`, beside the session cache,
all keys optional:

```toml
gateway    = "openshell"                              # unset: the active one
repo       = "https://github.com/octocat/Hello-World" # `sbx new` with no --repo
base       = "develop"                                # unset: the remote's default
policy     = "feature-work"                           # a template, or a path to a YAML file
providers  = ["claude-oauth", "azure-pat"]            # credentials for a new session
repo_roots = ["~/dev", "~/work"]                      # where the picker looks
worktree_root = "~/.local/share/sbx/worktrees"        # where worktree sessions go
refresh    = "1s"                                     # how often the TUI reads the sandboxes

skills     = ["ship-pr"]                               # copied into every session

[[mcp]]                                               # one table per MCP server
name = "jira"                                         # see docs/mcp.md
url  = "http://mcp-atlassian:9000/mcp"
```

Everything in it is a *default*: a flag on the command line wins, and so does an
explicit choice in the create form. `sbx config` prints what is in force with
`*` for the file's answers and `-` for the built-in ones.

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

The one exception is `sbx doctor`, which is the command you reach for when
something is wrong: it reports the error as a failed check and carries on with
the defaults. It also checks the `providers` you named still exist at the
gateway, since a stale name is the quietest failure here -- the form does not
tick it, the sandbox comes up without the credential, and the clone fails for
what looks like an authentication problem several steps later.

`refresh` is one number rather than six because the intervals underneath it are
measured and related to each other; it scales all of them, so `"4s"` polls a
quarter as often (41 execs in a 30 second window became 13) and `"500ms"` twice
as often. 250ms to 60s -- below that the TUI's 100ms input tick becomes the
limit and the extra `git status` inside every sandbox buys nothing.

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


---

[← Documentation](README.md) · [README](../README.md)
