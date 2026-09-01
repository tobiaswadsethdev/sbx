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

`ls`, `diff`, `policy` and `events` -- the reading half. Creating, publishing and
attaching still act on the local machine, and will grow their remote halves
along with the desktop application.

## The WSL case

A server in WSL and a client on Windows is two networking arrangements that look
identical from inside the Linux side, and getting it wrong looks exactly like a
firewall problem:

* **mirrored** -- WSL shares the Windows network stack, and the client uses
  `localhost:17671`.
* **NAT**, the default -- the client uses the address of the WSL VM, *which
  changes every time WSL restarts*. Every restart then needs pairing again.

`sbx doctor` says which one is in force and what to dial:

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
