# Contributing

Thanks for looking. `sbx` is early and small, which makes it a good size to
contribute to: the whole thing is two crates, the test suite runs in under a
second, and almost all of it can be worked on without an OpenShell gateway
anywhere near your machine.

Issues, questions and pull requests are all welcome. If you are unsure whether
something is wanted, open an issue first -- that is cheaper for both of us than
a branch that turns out to be aimed somewhere else.

## What you need

To **build and test**: Rust 1.89 or newer, and nothing else. The suite is
hermetic on purpose -- no gateway, no Docker, no network.

To **run it against real sandboxes**: everything in
[docs/install.md](docs/install.md) -- Linux with systemd, OpenShell 0.0.110 and
its gateway, Docker 29.x, tmux. That is worth setting up if you are changing how
sessions are created, seeded or published; it is not worth setting up to fix the
diff pane's wrapping.

## The loop

```sh
cargo build
cargo test --workspace                                    # 539 tests
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p sbxd -- doctor                               # the CLI, from the tree
cargo run -p sbxd -- serve                                # the server, from the tree
```

CI runs exactly those last three checks on the **newest stable**, so
`rustup update stable` before pushing is what makes a green local run a green
pull request: clippy gains lints with each release, and one of them can turn CI
red on code nobody touched. The gateway contract lives in ignored tests that need a live gateway
and Docker, and creates and deletes real sandboxes labelled `sbx.test`:

```sh
cargo test -p openshell-client -- --ignored --test-threads=1
```

[docs/architecture.md](docs/architecture.md) is the map of the crates and
modules -- worth ten minutes before your first change.

## House style

The code has a voice, and matching it is most of what review here is about.

* **Say why, not what.** Every module starts with a `//!` block explaining what
  the module is *for* and which alternative was rejected. New modules get one;
  changed behaviour updates the existing one. If a comment could be deleted
  without losing information, delete it.
* **Tests stay hermetic.** A test that needs a gateway, Docker or a network goes
  behind `#[ignore]`. Pane classification is tested against captured specimens
  in `crates/sbx-core/tests/panes/`; add a specimen rather than a mock when you are
  teaching it a new agent state.
* **No I/O on a render path.** Gateway calls are subprocess round trips costing
  hundreds of milliseconds. They belong behind `sbx-core`, reached over `/rpc`;
  nothing that paints may make one.
* **Failures name their fix.** `sbxd doctor` checks and error messages both say
  what to do about the problem, not just that there is one. A misspelled config
  key is named back at the user; a stale provider is reported before it becomes
  a clone failure three steps later.
* **The isolation is the product.** A change that widens what a sandbox can
  reach needs to be visible in the policy pane and defensible in the docs. If it
  is a hole, [docs/mcp.md](docs/mcp.md) is the tone to aim for -- say plainly
  what it costs.
* `cargo fmt` decides formatting. Don't argue with it in review.

## Pull requests

* One change per pull request. A drive-by rename in the same diff as a bug fix
  makes both harder to review.
* Say **why** in the description: what was wrong, what you did about it, and how
  you know it works. The pull request template asks for exactly that.
* Add tests for behaviour you change. If the change is genuinely untestable
  without a gateway, say so in the description and describe what you ran by hand
  -- `docs/manual-loop.md` is the shape that takes.
* Update the docs in the same pull request. User-visible behaviour lives in
  `docs/`, and the README links to it.
* Keep `cargo fmt` and `cargo clippy -- -D warnings` clean.

Commit messages: a short imperative summary, then a body explaining the
reasoning if the change is not obvious. `git log` here is a record of decisions
rather than a list of files touched, and [PLAN.md](PLAN.md) tracks the larger
increments.

## Releasing

Releases are what `install.sh` and `sbxd update` install, and both find them by
name. Cutting one is a dispatch of **Tag a release**
(`.github/workflows/tag.yml`) from the Actions tab, with `patch`, `minor` or
`major` -- or an exact version if the increment is not the point. It works out
the next version, rewrites the seven files that carry it, commits that as
`Release vX.Y.Z`, tags it, pushes both, and hands the tag to `release.yml`.
`dry_run` does everything except the pushing and prints the diff, which is the
cheap way to check a bump before it is permanent.

Seven files, because the version is written down in four formats across two
cargo workspaces:

| | |
| --- | --- |
| `Cargo.toml`, `Cargo.lock` | the workspace version every crate inherits |
| `apps/desktop/package.json`, `package-lock.json` | the front end |
| `apps/desktop/src-tauri/Cargo.toml`, `Cargo.lock` | the desktop crate, its own workspace |
| `apps/desktop/src-tauri/tauri.conf.json` | what Windows shows in Add or Remove Programs |

Bumping them by hand is the reason this is a workflow. `v0.3.0` was tagged with
all seven still reading `0.2.0`, and `sbxd update` refuses a release whose binary
reports a different version than the tag claims -- so the current `latest` is
one no installed copy can update to. The workflow writes all seven from one
number and refuses to tag if the diff touches anything else.

Doing it by hand still works, and is still the fallback if Actions is down:

```sh
# bump the seven files above, commit them, then:
git tag v0.2.0 && git push origin v0.2.0
```

A tag pushed from a laptop starts `release.yml` directly. One pushed by
`tag.yml` does not -- GitHub does not run workflows on events raised with the
built-in `GITHUB_TOKEN` -- which is why that workflow dispatches this one
explicitly rather than relying on the push. It needs no personal access token
to do it.

`.github/workflows/release.yml` builds a static musl binary for
`x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`, packs each as
`sbxd-<tag>-<target>.tar.gz` with `sbxd` flat at the root, and publishes them
with one `SHA256SUMS` covering both. Three files have to agree about that name
and about what is inside the archive -- the workflow, `install.sh` and
`crates/sbx-core/src/update.rs` -- and a test in `update.rs` fails if they ever
stop agreeing, so a rename in one of them is caught locally rather than by
someone's broken install.

The asset was `sbx-<tag>-<target>.tar.gz` until v0.4.0, and carried `sbx`.
Nothing installed before then can update across that rename: `sbx update`
replaces the binary it is running, and there is no longer an `sbx` to replace
it with. It fails saying the release has no asset by the name it wants, which
is true.

**The way across is one `install.sh`, from any release before v0.4.0.** There
is no update path that crosses it. The `sbxd` that v0.3.1 installs is the
server alone -- `update` did not move into it until v0.4.0 -- so it cannot
fetch its own successor either. What v0.3.1 is for is narrower and still worth
having: it is the last release `sbx update` can reach at all, so nobody is left
sitting on the v0.3.0 that no installed copy could update to.

Until the first tag exists there is nothing to download, and both installers
say so and fall back to building from source. That is the intended behaviour,
not a gap to work around.

### The signing keys

Two, for two different jobs, and both optional in the sense that a release
without them still builds:

| secret | what it is for | without it |
| --- | --- | --- |
| `WINDOWS_CERTIFICATE`, `WINDOWS_CERTIFICATE_PASSWORD` | code-signing the installer, so SmartScreen does not warn about it | the installer is unsigned; the checksums are still the integrity story |
| `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | signing `latest.json`, which is what an installed window checks before replacing itself | no `latest.json` is published and the window never offers to update |

The updater key is the one to be careful with. Its public half is compiled into
every window ever shipped (`plugins.updater.pubkey` in `tauri.conf.json`), and
a window will not install an update signed by anything else -- so **losing the
private key means no already-installed window can ever be updated again**,
whatever is published. Changing it has the same effect on every copy already
out there. Back it up somewhere that is not this repository.

`createUpdaterArtifacts` makes `tauri build` *fail* without the key rather than
skip the signing, so `release.yml` turns that option back off when the secret
is absent. That is what lets a fork cut a release at all.

## Reporting bugs

The single most useful thing to include is `sbxd doctor` output -- it captures
the versions and half the environment problems at once. The issue templates ask
for that, plus what you expected and what happened instead.

Security problems are different: please don't open a public issue. See
[SECURITY.md](SECURITY.md).

## Licence

By contributing you agree that your contributions are licensed under the
[Apache License 2.0](LICENSE), the same terms as the rest of the project.
