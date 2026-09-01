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
cargo test --workspace                                    # 403 tests
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo run -- doctor                                       # the CLI, from the tree
cargo run                                                 # the TUI, from the tree
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
* **No I/O on the render thread.** Gateway calls belong to `tui/worker.rs`; the
  UI sends a `Request` and drains an `Update`.
* **Failures name their fix.** `sbx doctor` checks and error messages both say
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

Releases are what `install.sh` and `sbx update` install, and both find them by
name. Tagging is the whole process:

```sh
# bump `version` in the workspace Cargo.toml, commit it, then:
git tag v0.2.0 && git push origin v0.2.0
```

`.github/workflows/release.yml` builds a static musl binary for
`x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`, packs each as
`sbx-<tag>-<target>.tar.gz` with the binary flat at the root, and publishes them
with one `SHA256SUMS` covering both. Three files have to agree about those names
-- the workflow, `install.sh` and `crates/sbx-core/src/update.rs` -- and a test in
`update.rs` fails if they ever stop agreeing, so a rename in one of them is
caught locally rather than by someone's broken install.

Until the first tag exists there is nothing to download, and both installers
say so and fall back to building from source. That is the intended behaviour,
not a gap to work around.

## Reporting bugs

The single most useful thing to include is `sbx doctor` output -- it captures
the versions and half the environment problems at once. The issue templates ask
for that, plus what you expected and what happened instead.

Security problems are different: please don't open a public issue. See
[SECURITY.md](SECURITY.md).

## Licence

By contributing you agree that your contributions are licensed under the
[Apache License 2.0](LICENSE), the same terms as the rest of the project.
