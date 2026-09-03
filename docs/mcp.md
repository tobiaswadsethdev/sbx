# MCP servers

The agents can be given MCP servers, and the servers run **on the host, in their
own containers, holding their own credentials**. Nothing about Jira or Azure
DevOps ever lands on a sandbox filesystem; the sandbox is granted one endpoint
per server, and the grant is per-binary like every other rule here:

```
claude → POST http://mcp-azure-devops:9001/mcp   ALLOWED  [policy:allow_mcp_azure_devops_9001]
curl   → POST http://mcp-azure-devops:9001/mcp   DENIED   [binary '/usr/bin/curl' not allowed]
```

Same host, same port, different binary -- measured from inside a session, not
described. Claude Code 2.x is a native binary, so `/usr/local/bin/claude` is a
rule only the agent satisfies. That is sharper than the registry rules in
`net-open.yaml`, where npm's kernel-resolved exe is `/usr/bin/node` and the rule
cannot tell an agent from anything else JavaScript in the sandbox.

## Two shapes: one you run, one the server runs

Name them in the config file, one table each. A table has either a `url` -- a
server somebody else operates -- or an `image`, which makes it **managed**:
`sbxd` starts the container, restarts it, holds its secrets and can say what it
is doing.

```toml
# Managed: described here, run by sbxd.
[[mcp]]
name    = "jira"
image   = "ghcr.io/sooperset/mcp-atlassian:latest"
port    = 9000
args    = ["--transport", "streamable-http", "--port", "9000"]
env     = { JIRA_URL = "https://your-org.atlassian.net", JIRA_USERNAME = "you@example.com" }
secrets = ["JIRA_API_TOKEN"]              # names; the values live on the server

# Yours: a url, exactly as before. Nothing here starts or stops it.
[[mcp]]
name = "azure-devops"
url  = "http://mcp-azure-devops:9001/mcp"
transport = "http"                        # or "sse"; http is the default
```

**A managed entry has no url, and that is the point.** It is reachable at
`http://sbx-mcp-<name>:<port>/mcp` because the thing that named the container is
the thing that joined it to the gateway's network -- so the two mistakes a
hand-written url invites, a name no sandbox can resolve and a `localhost` that
means the sandbox itself, are not reachable from there. Nothing is published on
the host: only sandboxes on that network need to reach it.

The keys that belong to a managed entry are refused beside a `url` rather than
ignored, and an entry with both is refused outright -- it would be a url
pointing somewhere other than the container beside it, which nobody notices
until an agent reports a dead tool.

### Secrets

The values live on the server, in `$XDG_STATE_HOME/sbx/secrets.json`, 0600 --
beside the pairing tokens and the TLS key, and protected exactly as well. The
config file holds only the *names*.

**A value goes in and never comes back out.** The window can store one and forget
one, and the protocol carries names and whether each is set; there is no request
that returns a value and there will not be one. From the server itself:

```sh
printf %s "$JIRA_API_TOKEN" | sbxd secret JIRA_API_TOKEN   # stdin, not an argument
sbxd secrets                                               # the names, never the values
sbxd secret JIRA_API_TOKEN --forget
```

Stdin rather than an argument on purpose: an argument lands in the shell history
and in `ps` output, and this is a credential a container will hold for months.
The same care applies when `sbxd` starts the container -- the value is put in the
child's environment and the argument list carries only the name.

This is not encryption at rest, and calling a file `secrets.json` invites the
assumption that it is. Anyone who can read it can already run commands as the
user whose containers would use it; a key stored beside the thing it encrypts
would be theatre.

### Starting and stopping

`sbxd` brings every managed container up when it starts, and before seeding a
session. Anything already running is left alone -- restarting it would drop the
agent connections of every live session using it. The **integrations** screen in
the window has the buttons; a headless server has the same thing:

```sh
sbxd mcp                                # the catalog, and what each one is doing
sbxd mcp --action start   jira          # bring one up
sbxd mcp --action restart jira          # recreate it from the catalog entry
sbxd mcp --action stop    jira
```

`restart` recreates the container from the config file rather than restarting the
one that is there, which is what you want after changing a secret, an argument or
the image tag: the container is the *deployment* of a catalog entry, not a thing
with a life of its own.

The states are worth knowing, because two of them look like health and are not:

| | |
| --- | --- |
| `running` | up, and on the gateway's network -- the only sense in which a sandbox can reach it |
| `crashing` | started, exited, started again. `--restart unless-stopped` means Docker reports a container it is restarting as *running*, so this is measured from the restart count and shows the container's last output |
| `detached` | running, but not on the `openshell-docker` network. Fine in `docker ps`, unreachable from every sandbox |
| `stopped` | it exists and is not running, with its last output |
| `absent` | never started, or stopped from here |
| `external` | a `url` entry: whoever runs it started it, and this server has no say |

## Running one yourself

Everything below is the `url` shape: your container, your `docker run`, your
problem when the host reboots. It is still the right answer for a server that
needs more than an image and a port -- a shim, a mount, a second process -- and
it is what the managed shape was measured against.

The url is what the **sandbox** sees, which is not what your browser sees.
`localhost` in there is the sandbox itself and is refused when the file is read,
because it is correct on the host, wrong in the sandbox, and invisible until an
agent is running. Two addresses work instead:

* **the container's name**, when it has joined the gateway's own Docker network
  with `--network openshell-docker`. Docker's embedded DNS resolves it even
  though the sandbox has no DNS of its own, because the proxy does the
  resolving -- and nothing is published on the host at all. This is the shape to
  prefer, and the one a managed entry gives you without asking.
* **`host.openshell.internal`**, which every sandbox already has in `/etc/hosts`
  pointing at the bridge gateway, for a server that is not in a container or is
  in one that cannot join another network. Publish to the bridge address rather
  than to `127.0.0.1`, or the sandbox cannot reach it.

Jira and Confluence, with the credentials staying in the container:

```sh
docker run -d --name mcp-atlassian --network openshell-docker \
  -e JIRA_URL=https://your-org.atlassian.net \
  -e JIRA_USERNAME=you@example.com -e JIRA_API_TOKEN="$JIRA_API_TOKEN" \
  ghcr.io/sooperset/mcp-atlassian:latest --transport streamable-http --port 9000
```

Azure DevOps needs one extra part: `@azure-devops/mcp` speaks stdio only, so it
runs behind an HTTP shim. Its `pat` mode reads `PERSONAL_ACCESS_TOKEN`, and wants
the base64 of `:<pat>` -- it decodes the value and drops everything up to the
first colon, which is Azure DevOps' usual empty-username Basic auth:

```sh
docker run -d --name mcp-azure-devops --network openshell-docker \
  -e PERSONAL_ACCESS_TOKEN="$(printf ':%s' "$AZURE_DEVOPS_PAT" | base64 -w0)" \
  node:22-alpine npx -y supergateway \
    --stdio "npx -y @azure-devops/mcp <org> -a pat" \
    --outputTransport streamableHttp --port 9001 --stateful
```

Both serve `/mcp`. The Azure DevOps one was run against a real session while
writing this -- the agent reported `azure-devops: ... ✔ Connected`, with Azure
DevOps MCP 2.9.0 answering behind the shim, and the denial above is `curl` in
that same sandbox. The Atlassian one was started and answered on `/mcp` with
placeholder credentials; its own flags are documented by that image.

Registration happens **inside the sandbox, before the agent starts** -- the
seeder runs `claude mcp add --scope user` as its own `mcp` step, because the
agent reads its servers at startup and registering them afterwards would leave
the first session of every sandbox without tools. The endpoints are opened in
one `policy update` at creation, so the rules are loaded before anything can use
them. A session records the servers it was created with, and the facts pane
lists them by name; changing the file changes the next session, not a running
one.

`sbx doctor` checks each of them, because a container that is not running -- or
one running but not attached to the gateway's network -- produces a session whose
agent reports its tools as **needing authentication**, which sends you looking in
entirely the wrong direction:

```
[ warn ] mcp          jira: there is no container named `mcp-atlassian`, so no sandbox can resolve that url
         fix: a managed one starts from the window's integrations screen; one of your own can be attached with `docker network connect openshell-docker <container>`, or its url fixed in the config file
```

A managed entry is asked of the same code the integrations screen uses, so a
check that passes here cannot disagree with a screen that says something is
wrong.

**What this costs.** An MCP server is a hole in the sandbox, and worth being
plain about -- which is why the sentence below is also in the window, beside the
list, rather than only here where nobody re-reads it: the agent gains everything the server can do, using the host's
credentials, and the gateway can only see it as `POST /mcp`. Every MCP call is
the same request shape, so the method/path rules that make the git endpoints
sharp buy nothing here -- a server that can transition Jira issues means a
sandboxed agent can transition Jira issues. That is a fine trade for Jira and
Azure DevOps, whose blast radius is a work item. It is a terrible one for a
filesystem or Docker MCP server on the host, which would be a straight sandbox
escape, and sbx cannot tell the difference for you.

The transport is not a problem the way it might look: streaming responses are
not buffered by the inspecting proxy. An SSE stream emitting an event a second
arrived event by event, a second apart, inside the sandbox.


---

[← Documentation](README.md) · [README](../README.md)
