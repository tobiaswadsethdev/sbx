## What this changes

<!-- One or two sentences. What was wrong or missing, and what this does about it. -->

## Why

<!-- The reasoning. Which alternative you rejected, if there was one. This is the
     part review here cares about most, and it usually belongs in a module doc too. -->

## How it was checked

<!-- Tests you added or changed. If it cannot be tested without a live gateway,
     say what you ran by hand and what you saw. -->

- [ ] `cargo test --workspace` passes
- [ ] `cargo fmt --all --check` is clean
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean
- [ ] Docs updated, if this changes behaviour anyone can see
- [ ] This does not widen what a sandboxed agent can reach -- or it does, and the
      trade is explained above and visible in the policy pane
