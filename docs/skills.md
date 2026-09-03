# Skills

Your skills do not follow you into a sandbox: it has its own `HOME`, so a fresh
one has none. Name them in the config file and every new session gets them:

```toml
skills = ["ship-pr", "~/dev/notes/.claude/skills/changelog"]
```

A bare name is one of your own, under `~/.claude/skills` (or
`$CLAUDE_CONFIG_DIR/skills`). Anything with a `/` in it is a path, so a skill
that lives in a repository can be pointed at where it actually is.

**It is a copy, and it cannot be anything else.** A symlink does not cross into
a sandbox and a bind mount would hand it the rest of `$HOME` -- the isolation is
the whole product. What the config file holds is the *pointer*, which buys the
part of a symlink that is actually wanted: edit the original, and the next
session gets the edit. A running session keeps what it was created with, and its
record and facts pane say what that was.

The whole directory travels, not just the manifest: `SKILL.md` beside its
scripts, references and templates, packed with `tar` on the host and unpacked
into `/sandbox/.claude/skills` as a seeder step before the agent starts. Symlinks
inside are followed, so a skill that is itself a link arrives as its contents.
A skill above 256KiB packed is refused rather than silently making the create
fail on an over-long command line -- at that size something has a virtualenv in
it by accident.

A skill that is missing at create time costs the skill, not the session: it is a
warning, and `sbx doctor` says so beforehand, since a session that quietly comes
up without one looks like the agent forgetting how to do something it used to
know.

```
[ warn ] skills       ship-pr: /home/you/.claude/skills/ship-pr has no SKILL.md, so the agent would not load it
```

## When the sessions are on another machine

"The host" means one machine until there is a server, and then it means two: the
sessions run where `sbxd` is, and your skills are on the machine with the window
on it. A path in the server's config file cannot reach them.

So the server keeps a **library** at `$XDG_DATA_HOME/sbx/skills`, and the window
pushes this machine's own `~/.claude/skills` into it. A session is given both --
the paths in the server's config file, and everything in the library those do not
already name.

The push happens from the **integrations** screen, and again automatically before
every create, which is what keeps the pointer-not-copy property across the extra
hop: editing a skill on your laptop still means the next session gets the edit.
Every skill the agent would load goes, not a selection -- a list to maintain in
the window would go stale the first time you add a skill and forget.

The reading and the packing happen on the *client's* side of the bridge, because
that is where the directory is; the server unpacks each one into a staging
directory and checks it before it lands, since a tar arriving from another
machine is a program's output rather than a promise. An archive that unpacks as
two things, as something other than its own name, or without a `SKILL.md` is
refused and nothing is left behind.

From the server, to see what has arrived:

```sh
sbxd skills          # what the library holds, and where each came from
```

The library is a cache of a directory on another machine, so removing an entry
from the screen removes the server's copy and nothing of yours. Uploaded skills
are global, like the configured ones, and for the same reason: this is what an
agent of yours knows how to do, not a per-session choice. A running session keeps
what it was handed.


---

[← Documentation](README.md) · [README](../README.md)
