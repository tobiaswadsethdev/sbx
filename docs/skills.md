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


---

[← Documentation](README.md) · [README](../README.md)
