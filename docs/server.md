# The server

`sbx` drives the sandboxes on the machine it runs on. `sbxd` lets something else
drive them: another machine on the network, a cloud box, or -- the case this was
built for -- a Linux server inside WSL with the client out on Windows.

The sessions are the same sessions. One gateway, one cache, one set of
sandboxes, whether the thing asking is a terminal on that machine or a client
somewhere else.

```
   Windows                          WSL / a VM / a box on the LAN
   +-----------+                    +-----------------------------+
   |  sbx      |  TLS, one token    |  sbxd  ->  openshell gateway|
   |  (client) | -----------------> |            -> sandboxes     |
   +-----------+                    +-----------------------------+
```

## Pairing

On the server:

```sh
sbxd pair desktop            # one string, shown once
sbxd serve                   # or run it under systemd; see below
```

`pair` prints a single line:

```
sbx://box.lan:17671/8f3c...e21a#d8faa48b...e140
```

That is the address, a token, and the fingerprint of the certificate the server
will present. On the client:

```sh
sbx connect 'sbx://box.lan:17671/8f3c...e21a#d8faa48b...e140' --name work
sbx --server=work ls
```

`--server` alone is enough when only one server is paired. The name has to be
attached with `=`, because `sbx --server work ls` cannot be told apart from a
server called `work`.

```sh
sbx remotes                  # what this machine is paired with
sbx remotes --forget work    # stop being
sbxd tokens                  # what the server accepts
sbxd revoke desktop          # stop accepting one, immediately
```

Both directions take effect at once: a token minted while the server is running
works on the next request, and a revoked one stops working on the next request.
Neither needs a restart.

## What works over a connection

Nearly everything, now. Listing and reconciling sessions; creating one; the
policy and the event feed; the working copy's files; git, including staging,
commit, push, pull and fetch; the diff and the review comments that go back to
the agent; and the agent's terminal and any shells beside it, streamed.

Two things are still the local machine's. **Attaching** with `sbx attach` hands
*this* terminal to the agent, which is a thing about the process you are
sitting in rather than a request; the desktop application's terminal is the
remote equivalent and it works over the connection. **Publishing** with
`sbx publish` has no remote half yet.

Reading is `/rpc`, one request and one answer. The three things a client wants
*told* -- the agent's screen, the gateway's decisions as it makes them, and the
terminal -- are a websocket on `/ws`, multiplexed by channel, because polling
them would be a handshake per session per second to hear that nothing had
changed.

## Two machines

The server needs to listen somewhere the client can reach, and the pairing
string needs to name that address.

On the server:

```sh
# Listen on the network rather than on loopback. `--bind 0.0.0.0` is every
# interface; a single address is better where there is a choice.
sbxd serve --bind 0.0.0.0

# In another shell: mint a token, and name the address the *client* should dial.
sbxd pair laptop --host box.lan
```

On the client, paste the string it printed:

```sh
sbx connect 'sbx://box.lan:17671/8f3c...#d8fa...' --name work
sbx --server=work ls
```

**`--host` is the flag that matters, and leaving it out is the usual failure.**
The pairing string carries an address, and without `--host` that address is the
machine's own `/etc/hostname` -- which is frequently not a name the client can
resolve, and on a Debian-family box resolves *on the server itself* to
`127.0.1.1` while `sbxd serve` is bound to `127.0.0.1`. The result is
`Connection refused` from a server that is running perfectly well, on the same
machine. Pass the address you want the client to dial:

```sh
sbxd pair laptop --host 127.0.0.1     # same machine
sbxd pair laptop --host box.lan       # over the network by name
sbxd pair laptop --host 10.0.0.7      # ... or by address
```

**The certificate's names do not matter to `sbx`.** The client judges a server
by the fingerprint in the pairing string and by nothing else -- `verify_server_cert`
ignores the name it was given -- so there is no "certificate is not valid for
this host" to run into, whatever address you dial. That is the point of pinning:
a self-signed certificate has no authority to appeal to, and a name check
against one proves nothing anyway.

`--san` is therefore for *other* clients: `curl`, a browser, anything that does
check names against a certificate. It is repeatable:

```sh
sbxd serve --bind 0.0.0.0 --san sbx.internal
```

Two things to know if you do use it. The certificate is generated once and then
reused whatever the SANs say, so a `--san` added after the first start does not
appear in it. And `sbxd pair` generates the certificate too, with the defaults
only -- so pairing before the first `serve --san` locks the extra name out.
Delete `~/.local/state/sbx/cert.pem` and `key.pem` and start again, which means
pairing again: the fingerprint every client holds has changed.

Two more things that are not sbx's to fix but look exactly like it:

* **The port has to be open.** 17671/tcp on the server's firewall.
* **Both ends need the same protocol version.** `GET /version` answers without a
  token, so a client says "this server speaks 2, I speak 1" rather than failing
  in the middle of a request. `sbx doctor` on the client checks every paired
  server, which is where a moved address, a revoked token or a version skew
  shows up.

The desktop application reads the same paired servers as the CLI: pair once with
`sbx connect` and the window lists that server without being told again. It can
also *be* the thing that pairs -- paste the string into its **servers** dialog,
which runs the same checks and writes the same file. That is what a Windows
client does, having no `sbx` to run:
[desktop.md](desktop.md#connecting-it-to-a-server).

## The WSL case

A server in WSL and a client on Windows is two networking arrangements that look
identical from inside the Linux side, and getting it wrong looks exactly like a
firewall problem:

* **mirrored** -- WSL shares the Windows network stack, and the client uses
  `localhost:17671`.
* **NAT**, the default -- the client uses the address of the WSL VM, *which
  changes every time WSL restarts*. Every restart then needs pairing again.

Only the window goes on the Windows side -- there is no `sbx` there, and the
pairing is done from the window itself. [install.md](install.md#windows) is the
installer.

`sbx doctor`, on the Linux side, says which one is in force and what to dial:

```
[  ok  ] wsl          mirrored networking: a client on Windows uses localhost:17671
```

Mirrored is worth turning on for this, in `%USERPROFILE%\.wslconfig`:

```ini
[wsl2]
networkingMode=mirrored
```

`sbx doctor` also checks every paired server, which is where an address that has
moved or a token that was revoked shows up:

```
[ FAIL ] servers      work: could not reach the server: box.lan:17671: Connection refused
         fix: check it is running, and that `box.lan:17671` is the address this
              machine should dial. `sbx remotes --forget work` drops it
```

## What this costs

**An authenticated client can create containers on the server's host, which
makes a token equivalent to a login to that machine.** That is not a
qualification, it is the shape of the thing: the point of the server is to start
sandboxes, and starting sandboxes is a privileged act. Treat a pairing string
the way you would treat an SSH private key.

`sbxd` therefore listens on `127.0.0.1` unless told otherwise, and says so when
told otherwise:

```
listening on https://0.0.0.0:17671 -- reachable from the network. An
authenticated client can create containers on this host, so treat a token as a
login to it
```

Two things follow from that being the deal:

* **Tokens are named and revocable.** One per client, so losing a laptop costs
  that laptop's access and nothing else. They are 32 bytes from the OS, kept as
  a SHA-256 hash, and compared in constant time -- the file on the server holds
  nothing that can be presented as a credential.
* **The certificate is pinned, not trusted.** There is no authority to appeal
  to, so the client checks the server's certificate against the fingerprint in
  the pairing string and accepts nothing else. That covers the *first*
  connection as well as later ones, which ordinary trust-on-first-use does not.
  If the server is rebuilt and generates a new certificate, the client refuses
  and says both fingerprints; pair again.

Keys, tokens and saved connections live in `$XDG_STATE_HOME/sbx` (usually
`~/.local/state/sbx`), owner-readable only -- not in `~/.config/sbx` beside the
session cache, because config directories are the ones people sync between
machines.

## Running it under systemd

Same shape as the gateway's own unit, as a user service:

```ini
# ~/.config/systemd/user/sbxd.service
[Unit]
Description=sbx server
After=openshell-gateway.service
Wants=openshell-gateway.service

[Service]
ExecStart=%h/.local/bin/sbxd serve
Restart=on-failure

[Install]
WantedBy=default.target
```

```sh
systemctl --user enable --now sbxd
loginctl enable-linger "$USER"    # so it survives logging out
```

`sbxd` does not fail to start when the gateway is down: a server you cannot
reach is a server that cannot tell you why. It says so and carries on.

---

[← Documentation](README.md) · [README](../README.md)
