#!/bin/sh
# Install sbx, without a checkout and without a Rust toolchain.
#
#   curl -fsSL https://raw.githubusercontent.com/tobiaswadsethdev/sbx/main/install.sh | sh
#
# It fetches the newest release for this machine, checks it against the
# published SHA256SUMS, and puts the binary somewhere on PATH. Nothing else:
# the prerequisites sbx needs at runtime -- OpenShell, its gateway, Docker,
# tmux -- are what `sbx doctor` is for, and it is run at the end to say which
# of them are missing.
#
# Options, as flags or as environment variables:
#
#   --version v0.2.0   SBX_VERSION    a specific release; default the newest
#   --bin-dir DIR      SBX_BIN_DIR    where to install; default ~/.local/bin
#   --from-source      SBX_FROM_SOURCE=1
#                                     build with cargo instead of downloading
#
# Piped into sh, flags go after `-s --`:
#
#   curl -fsSL .../install.sh | sh -s -- --bin-dir ~/bin
set -eu

REPO="tobiaswadsethdev/sbx"
VERSION="${SBX_VERSION:-}"
BIN_DIR="${SBX_BIN_DIR:-}"
FROM_SOURCE="${SBX_FROM_SOURCE:-}"

say() { printf '%s\n' "$*"; }
err() { printf 'install.sh: %s\n' "$*" >&2; }
die() {
    err "$*"
    exit 1
}

# A heredoc rather than the comment block above it, because piped into `sh`
# there is no script file to read the comments back out of.
usage() {
    cat <<'USAGE'
install.sh -- install sbx without a checkout and without a Rust toolchain

  curl -fsSL https://raw.githubusercontent.com/tobiaswadsethdev/sbx/main/install.sh | sh

Options, as flags or as environment variables:

  --version v0.2.0   SBX_VERSION       a specific release; default the newest
  --bin-dir DIR      SBX_BIN_DIR       where to install; default ~/.local/bin
  --from-source      SBX_FROM_SOURCE=1 build with cargo instead of downloading

Piped into sh, flags go after `-s --`:

  curl -fsSL .../install.sh | sh -s -- --bin-dir ~/bin
USAGE
    exit 0
}

while [ $# -gt 0 ]; do
    case "$1" in
        --version)
            VERSION="${2:-}"
            shift 2 || die "--version needs a tag, like v0.1.0"
            ;;
        --version=*) VERSION="${1#*=}" ; shift ;;
        --bin-dir)
            BIN_DIR="${2:-}"
            shift 2 || die "--bin-dir needs a directory"
            ;;
        --bin-dir=*) BIN_DIR="${1#*=}" ; shift ;;
        --from-source) FROM_SOURCE=1 ; shift ;;
        -h|--help) usage ;;
        *) die "unknown option: $1  (--help for the list)" ;;
    esac
done

[ -n "$BIN_DIR" ] || BIN_DIR="${HOME}/.local/bin"

have() { command -v "$1" >/dev/null 2>&1; }

# ---------------------------------------------------------------- from source

# The fallback, and it is a real one rather than an apology: before the first
# tagged release, and on any machine no release is built for, this is the whole
# installation. `--locked` so it builds against the versions the tree was
# tested with.
build_from_source() {
    have cargo || die "no releases to install and no cargo to build with.
     fix: install Rust from https://rustup.rs, then re-run this script"
    say "==> building from source with cargo (this takes a few minutes)"
    cargo install --git "https://github.com/${REPO}" sbx --locked
    say "==> installed to $(cargo_bin)/sbx"
    finish "$(cargo_bin)"
}

cargo_bin() { printf '%s\n' "${CARGO_HOME:-$HOME/.cargo}/bin"; }

# ------------------------------------------------------------------- platform

# Only Linux, and only the two architectures releases are built for. The
# isolation sbx provides is kernel-enforced, so there is nothing to install
# anywhere else -- but say that rather than failing on a 404 later.
detect_target() {
    os="$(uname -s)"
    [ "$os" = "Linux" ] || die "sbx is Linux-only: the isolation is kernel-enforced.
     (this is $os)"
    case "$(uname -m)" in
        x86_64 | amd64) printf 'x86_64-unknown-linux-musl\n' ;;
        aarch64 | arm64) printf 'aarch64-unknown-linux-musl\n' ;;
        *) die "no release is built for $(uname -m)
     fix: --from-source, which needs a Rust toolchain" ;;
    esac
}

# ------------------------------------------------------------------- releases

# The newest release's tag, read out of the API response without jq: one field,
# and a dependency for it would be a worse trade than this sed.
latest_tag() {
    curl -fsSL --connect-timeout 5 --max-time 20 \
        -H 'Accept: application/vnd.github+json' \
        "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null |
        sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' |
        head -n 1
}

# ---------------------------------------------------------------------- steps

install_release() {
    target="$(detect_target)"

    tag="$VERSION"
    if [ -z "$tag" ]; then
        say "==> looking for the newest release"
        tag="$(latest_tag || true)"
    fi
    if [ -z "$tag" ]; then
        err "no published release found for ${REPO}"
        err "falling back to building from source"
        build_from_source
        return
    fi

    asset="sbx-${tag}-${target}.tar.gz"
    base="https://github.com/${REPO}/releases/download/${tag}"

    tmp="$(mktemp -d "${TMPDIR:-/tmp}/sbx-install.XXXXXX")"
    trap 'rm -rf "$tmp"' EXIT INT TERM

    say "==> downloading ${asset}"
    curl -fsSL --connect-timeout 5 --max-time 300 -o "${tmp}/${asset}" "${base}/${asset}" ||
        die "could not download ${base}/${asset}
     fix: check the tag exists at https://github.com/${REPO}/releases"
    curl -fsSL --connect-timeout 5 --max-time 60 -o "${tmp}/SHA256SUMS" "${base}/SHA256SUMS" ||
        die "release ${tag} publishes no SHA256SUMS, so nothing can be verified"

    # An unverified binary is not installed. `--ignore-missing` because the
    # file covers every architecture's asset and only one was downloaded.
    say "==> verifying"
    (cd "$tmp" && sha256sum --check --ignore-missing --status SHA256SUMS) ||
        die "checksum mismatch for ${asset} -- do not run it
     fix: report this at https://github.com/${REPO}/security/advisories/new"

    tar -xzf "${tmp}/${asset}" -C "$tmp" || die "could not unpack ${asset}"
    [ -f "${tmp}/sbx" ] || die "${asset} does not contain an sbx binary"

    mkdir -p "$BIN_DIR" || die "cannot create ${BIN_DIR}"
    # Copy next door and rename, rather than writing over the target: `install`
    # and `cp` both write in place, which fails with ETXTBSY when the binary
    # they are overwriting is one a TUI in another terminal is running. A
    # rename inside one directory replaces it atomically instead, and Linux is
    # content to rename over an executing binary.
    staged="${BIN_DIR}/.sbx-install.$$"
    cp "${tmp}/sbx" "$staged" 2>/dev/null && chmod 755 "$staged" || {
        rm -f "$staged"
        die "cannot write to ${BIN_DIR}
     fix: --bin-dir DIR for somewhere you own, or re-run with sudo"
    }
    mv -f "$staged" "${BIN_DIR}/sbx" || {
        rm -f "$staged"
        die "cannot replace ${BIN_DIR}/sbx"
    }

    say "==> installed ${tag} to ${BIN_DIR}/sbx"
    finish "$BIN_DIR"
}

# What to do next, and the one thing that silently goes wrong: a binary in a
# directory that is not on PATH looks like an install that did nothing.
finish() {
    dir="$1"
    case ":${PATH}:" in
        *":${dir}:"*) ;;
        *)
            say ""
            say "    ${dir} is not on your PATH. Add it:"
            say ""
            say "        export PATH=\"${dir}:\$PATH\""
            say ""
            ;;
    esac

    if [ -x "${dir}/sbx" ]; then
        say ""
        say "==> sbx doctor"
        # Never fatal: doctor exits non-zero when a prerequisite is missing,
        # which is the normal state of a machine that has just installed this
        # and is exactly what the output is for.
        "${dir}/sbx" doctor || true
        say ""
        say "Prerequisites and what to do about them:"
        say "    https://github.com/${REPO}/blob/main/docs/install.md"
    fi
}

# ----------------------------------------------------------------------- main

have curl || die "curl is needed to download anything.
     fix: install curl, or --from-source with a Rust toolchain"

if [ -n "$FROM_SOURCE" ]; then
    build_from_source
else
    have sha256sum || die "sha256sum is needed to verify the download (coreutils)"
    have tar || die "tar is needed to unpack the download"
    install_release
fi
