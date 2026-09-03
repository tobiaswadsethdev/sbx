# The task inbox

What your trackers say is assigned to you, in the window, with one button that
turns a ticket into a session — and a publish that comments the pull request
back onto it.

```
   PROJ-123   jira    In Progress   Add the changelog        [ inet ▾ ] start
   AB#1234    ado     Active        Order backfill throws…   [ inet ▾ ] start
   #45        github  open          Readme says the wrong…   [ sbx  ▾ ] start
```

GitHub, Azure DevOps and Jira, read **on the server, over REST, with the
credentials in the server's store**.

## REST here, MCP there

The agent may also have a Jira MCP server; this is not that, and the difference
is deliberate. REST is for what the *interface* shows: a list, on a timer,
rendered as rows. MCP is for what the *agent* gets: a tool it calls when it
decides to. They are different consumers with different failure modes — a list
that cannot be fetched is a pane with a message in it, a tool that cannot be
reached is a session whose agent gives up on a step — and one mechanism serving
both would serve both badly.

## Configuring one

A `[[tracker]]` table per tracker, in the **server's** config file. The
credential is named, never written: the value lives in the server's secret store
(see [mcp.md](mcp.md#secrets)), which is also where the window's integrations
screen puts it.

```toml
branch_prefix = "tobias"                  # what a work branch is named under

[[tracker]]
kind       = "jira"
site       = "https://your-org.atlassian.net"
email      = "you@example.com"            # Jira Cloud is Basic: email + API token
secret     = "JIRA_API_TOKEN"
on_publish = "Ready for Review"           # optional: where to move it

[[tracker]]
kind       = "azure-devops"
org        = "your-org"
project    = "YourProject"
secret     = "AZURE_DEVOPS_PAT"
on_publish = "Resolved"

[[tracker]]
kind   = "github"
repo   = "owner/name"                     # optional; omit for everything assigned to you
secret = "GITHUB_TOKEN"
```

Then store the credentials:

```sh
printf %s "$JIRA_API_TOKEN" | sbxd secret JIRA_API_TOKEN
sbx tasks                                 # the inbox, from a terminal
sbx --server=<name> tasks                 # ... or from a client
```

`sbx doctor` says when a tracker names a secret the store does not have, because
that produces an inbox **silently missing its rows** — which looks exactly like
having nothing assigned to you.

The default query is "assigned to me and not done", which is what an inbox is.
`query` replaces it: JQL for Jira, WIQL for Azure DevOps.

## What a ticket becomes

Starting from a row fills in three things, all decided on the server so both
front ends would agree:

| | |
| --- | --- |
| the task | `PROJ-123: Add the changelog` and the ticket's URL, so the agent's first instruction says why it exists |
| the session name | `proj-123-add-the-changelog`, cut to what a session name may be |
| the branch | `<branch_prefix>/PROJ-123-add-the-changelog` — the key keeps its case, because a tracker's commit hooks and your reviewers both look for `PROJ-123` |

**A ticket does not know which repository it is about.** A Jira issue names a
project and a work item names an area path; neither is a clone URL, and guessing
from a name would be wrong in exactly the cases where it matters. So the row
carries a project chooser: the tracker says what to do and you say where.

`branch_prefix` applies to every session, not only the ones from a ticket — it
is `sbx` unless the config file says otherwise, which is what every session's
branch has been until now.

## The round trip

A session started from a ticket records which ticket, and publishing writes back
to it:

* a comment with the pull request's URL, on the ticket;
* the status moved to `on_publish`, when one is configured.

Jira is moved by *transition*, matched by name against what that issue can
actually do from where it is — a workflow only offers some of them, so a name
that is not among the available ones comes back saying which are. Azure DevOps
is a `System.State` patch. GitHub has no status between open and closed, and
closing an issue because a pull request exists is a decision for whoever merges
it; a pull request whose body says `Fixes #45` does it on merge.

**Both halves are best-effort, and both say what happened.** By the time they
run, the branch is pushed and the pull request is open: a tracker that cannot be
written to costs a comment, not the publish, so it comes back as a warning
beside whatever git said. A publish with `--no-pr` writes nothing back, because
there is nothing to point at.

The record on the session is what makes this work minutes or days later, from a
different client, after the inbox has moved on — the ticket's tracker, id, key
and URL, which is everything a write-back is addressed with and nothing else.

## What it costs

The server holds a credential that can read your tickets and comment on them.
It is in the same store, with the same protection, as the pairing tokens and the
TLS key — and a pairing token was already a login to that machine. What is new
is the blast radius of *that machine* being compromised: it now includes leaving
comments and moving tickets. Scope the tokens to what the inbox needs (read
work items, add comments) rather than reusing an administrative one.

---

[← Documentation](README.md) · [README](../README.md)
