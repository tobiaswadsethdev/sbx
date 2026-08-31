# rust -- the standalone distribution, not rustup.
#
# rustup is the obvious choice and the wrong one here, for a reason that only
# shows up under this gateway. `$CARGO_HOME/bin/cargo` installed by rustup is a
# *proxy*: it execs the real cargo inside
# `$RUSTUP_HOME/toolchains/<channel>-<triple>/bin/cargo`, and the gateway matches
# on the kernel-resolved `/proc/<pid>/exe`. So a policy rule for cargo would have
# to name a path containing the host triple -- the same trap `net-open.yaml`
# documents for uv's managed python, where `pip install` is denied with a path
# nobody put in the policy. The standalone installer lays down a real binary at a
# path this file chooses, so `toolchain.rs` can name it and be right on every
# architecture.
#
# Nothing is lost that a sandbox could use anyway: rustup exists to switch
# toolchains, and /usr/local is read-only to the agent with no route to
# static.rust-lang.org. The toolchain is the image's business, exactly as the
# agent's own version is.
#
# The component list is explicit rather than `--without=rust-docs`, so what a
# variant carries is readable here: the compiler, the standard library, cargo,
# and the two subcommands an agent reaches for constantly. rust-docs alone is
# most of the download, and rust-analyzer is for an editor nobody is running in
# here.
#
# `--build-arg RUST_CHANNEL=beta` builds another channel. The version is resolved
# from the channel manifest and the tarball checked against the sha256 the
# publisher published beside it, for the reason the Claude Code step does the
# same: a rebuild has to be honest about what it installed.
ARG RUST_CHANNEL=stable
RUN set -eu; \
    arch="$(dpkg --print-architecture)"; \
    case "$arch" in \
        amd64) triple=x86_64-unknown-linux-gnu ;; \
        arm64) triple=aarch64-unknown-linux-gnu ;; \
        *) echo "no rust distribution for architecture $arch" >&2; exit 1 ;; \
    esac; \
    dist=https://static.rust-lang.org/dist; \
    curl -fsSL --retry 3 -o /tmp/channel.toml "$dist/channel-rust-$RUST_CHANNEL.toml"; \
    version="$(awk '/^\[pkg\.rust\]/{f=1} f&&/^version/{gsub(/"/,"");print $3; exit}' /tmp/channel.toml)"; \
    case "$version" in \
        [0-9]*.[0-9]*.[0-9]*) ;; \
        *) echo "not a rust version: $version" >&2; exit 1 ;; \
    esac; \
    echo "rust $version ($triple)"; \
    tarball="rust-$version-$triple.tar.gz"; \
    curl -fsSL --retry 3 -o "/tmp/$tarball" "$dist/$tarball"; \
    curl -fsSL --retry 3 -o /tmp/rust.sha256 "$dist/$tarball.sha256"; \
    echo "$(awk '{print $1}' /tmp/rust.sha256)  /tmp/$tarball" | sha256sum -c -; \
    mkdir -p /tmp/rust; \
    tar -xzf "/tmp/$tarball" -C /tmp/rust --strip-components=1; \
    /tmp/rust/install.sh \
        --prefix=/usr/local/rust \
        --disable-ldconfig \
        --components="rustc,rust-std-$triple,cargo,rustfmt-preview,clippy-preview"; \
    rm -rf /tmp/rust "/tmp/$tarball" /tmp/rust.sha256 /tmp/channel.toml; \
    for bin in cargo rustc rustfmt cargo-fmt cargo-clippy clippy-driver; do \
        ln -sf "/usr/local/rust/bin/$bin" "/usr/local/bin/$bin"; \
    done; \
    test -x /usr/local/rust/bin/cargo \
        || { echo "cargo is not at /usr/local/rust/bin/cargo, which the policy names" >&2; exit 1; }; \
    installed="$(rustc --version | awk '{print $2}')"; \
    test "$installed" = "$version" \
        || { echo "wanted rust $version, got $installed" >&2; exit 1; }; \
    cargo --version >/dev/null; \
    printf 'rust %s\n' "$version" >> /usr/local/share/sbx/toolchains

# CARGO_HOME under $HOME, which is the writable half of the sandbox.
#
# Not for the binaries -- those live in /usr/local/rust and are on PATH by
# symlink, read-only to the agent, which is the point. CARGO_HOME here is purely
# data: the registry index, the crate cache, `cargo install` output, and
# `config.toml` if the agent writes one. Leaving it at its default would put it
# in the same place; naming it is what makes the directory a decision rather than
# an accident, and it is the directory a `cargo fetch` fills.
#
# Set twice for the reason the base image sets its locale twice: `ENV` covers a
# shell opened by hand, `set-environment` in tmux.conf is what reaches the agent.
ENV CARGO_HOME=/sandbox/.cargo

RUN printf '%s\n' \
        'set-environment -g CARGO_HOME /sandbox/.cargo' \
    >> /etc/tmux.conf
