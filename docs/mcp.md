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

Name them in the config file, one table each:

```toml
[[mcp]]
name = "jira"
url  = "http://mcp-atlassian:9000/mcp"

[[mcp]]
name = "azure-devops"
url  = "http://mcp-azure-devops:9001/mcp"
transport = "http"                        # or "sse"; http is the default
```

The url is what the **sandbox** sees, which is not what your browser sees.
`localhost` in there is the sandbox itself and is refused when the file is read,
because it is correct on the host, wrong in the sandbox, and invisible until an
agent is running. Two addresses work instead:

* **the container's name**, when it has joined the gateway's own Docker network
  with `--network openshell-docker`. Docker's embedded DNS resolves it even
  though the sandbox has no DNS of its own, because the proxy does the
  resolving -- and nothing is published on the host at all. This is the shape to
  prefer.
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
         fix: start it, or attach it with `docker network connect openshell-docker <container>`; or fix its url in the config file
```

**What this costs.** An MCP server is a hole in the sandbox, and worth being
plain about: the agent gains everything the server can do, using the host's
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
