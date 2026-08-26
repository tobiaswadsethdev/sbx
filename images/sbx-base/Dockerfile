# syntax=docker/dockerfile:1
#
# sbx sandbox image.
#
# The community base image has git, node, python, uv, gh and claude, but no
# tmux. sbx runs each agent inside an in-sandbox tmux session so the agent
# survives losing its connection, and so its output can be scraped with
# capture-pane without depending on anything on the host.
#
# Build:  sbx image build   (or: docker build -t sbx-base:latest images/sbx-base)
FROM ghcr.io/nvidia/openshell-community/sandboxes/base:latest

USER root

# tmux    - keeps the agent alive independently of any connection
# less    - git's pager; without it git paging misbehaves under a pty
# jq      - reading agent JSON output
# ripgrep - agents reach for it constantly
RUN apt-get update && apt-get install -y --no-install-recommends \
        tmux \
        less \
        jq \
        ripgrep \
    && rm -rf /var/lib/apt/lists/*

# Claude Code, at the newest release rather than whatever the base image froze.
#
# The base image bakes in a native `claude` binary and updates on its own
# schedule -- it shipped 2.1.143 while 2.1.246 was current -- and the agent
# cannot fix that from inside: a sandbox under `feature-work` cannot reach the
# download host, and /usr/local/bin is not writable by the sandbox user anyway.
# So the version is the image's business.
#
# `latest` is resolved to a concrete version, either here or by the caller, and
# `--build-arg CLAUDE_VERSION=2.1.246` pins it when a specific one is wanted.
# `sbx image build` always resolves it on the host and passes it in, because
# docker would otherwise answer a rebuild from the cached layer and `latest`
# would quietly mean "whatever was newest the first time" -- which is the exact
# staleness this step exists to fix. `sbx doctor` compares the built image
# against the newest release and says when it has fallen behind.
#
# The release binary, not install.sh: the installer lands a launcher under
# `$HOME`, which here is the sandbox user's writable home, so the agent could
# replace its own binary. Overwriting the base image's copy in /usr/local/bin
# keeps it on PATH for every user and read-only to the agent.
#
# The checksum comes from the release manifest, so a truncated or substituted
# download fails the build instead of shipping. The `--version` check at the end
# catches the subtler failure: a manifest and a binary that disagree, leaving the
# old binary in place and the build green.
ARG CLAUDE_VERSION=latest
RUN set -eu; \
    arch="$(dpkg --print-architecture)"; \
    case "$arch" in \
        amd64) platform=linux-x64 ;; \
        arm64) platform=linux-arm64 ;; \
        *) echo "no claude release for architecture $arch" >&2; exit 1 ;; \
    esac; \
    base=https://downloads.claude.ai/claude-code-releases; \
    version="$CLAUDE_VERSION"; \
    if [ "$version" = latest ]; then version="$(curl -fsSL "$base/latest")"; fi; \
    case "$version" in \
        [0-9]*.[0-9]*.[0-9]*) ;; \
        *) echo "not a claude version: $version" >&2; exit 1 ;; \
    esac; \
    checksum="$(curl -fsSL "$base/$version/manifest.json" \
        | jq -re --arg p "$platform" '.platforms[$p].checksum')"; \
    curl -fsSL -o /tmp/claude "$base/$version/$platform/claude"; \
    echo "$checksum  /tmp/claude" | sha256sum -c -; \
    install -m 0755 -o root -g root /tmp/claude /usr/local/bin/claude; \
    rm -f /tmp/claude; \
    installed="$(claude --version | awk '{print $1}')"; \
    test "$installed" = "$version" \
        || { echo "wanted claude $version, got $installed" >&2; exit 1; }

# A long scrollback is what makes capture-pane useful for previews and, later,
# for detecting what the agent is waiting on.
#
# `default-size` is the width an unattached session gets, and unattached is how
# the agent spends most of its life -- so it is the width every status scrape
# reads. Claude Code's footer is a list of hints truncated to the pane, and at
# tmux's default 80x24 it already ends in an ellipsis: `esc to interrupt`, which
# is how a working agent is recognised, sits in the middle of that list and would
# be the next thing to fall off it. 200 columns is not a display choice; it keeps
# the markers on screen. Attaching resizes the window to the terminal as usual.
RUN printf '%s\n' \
        'set -g history-limit 50000' \
        'set -g status off' \
        'set -g mouse on' \
        'set -g escape-time 10' \
        'set -g default-size 200x50' \
    > /etc/tmux.conf \
    && chmod 0644 /etc/tmux.conf

# The agent does not manage its own version; this image does.
#
# Claude Code otherwise tries to update itself on startup and fails twice over:
# /usr/local/bin is root-owned, and no policy template lets a sandbox reach the
# download service. The visible cost was `✘ Auto-update failed · Run claude
# doctor` sitting on the agent's screen for the rest of the session, and a denied
# egress event in the policy feed with nothing behind it worth investigating.
#
# Set twice on purpose. `ENV` covers anything started in the sandbox, including a
# shell someone opens by hand; the same variable in `settings.json` is what
# actually reaches the agent, because the gateway does not pass the image's
# environment through to an exec.
ENV DISABLE_AUTOUPDATER=1

# Skip Claude Code's first-run onboarding.
#
# A fresh sandbox has a fresh HOME, so without this every session opens on the
# theme picker and then the "do you trust this folder?" prompt, and the agent
# never reaches its task. lastOnboardingVersion is pinned to the version baked
# into this image so a future upgrade can legitimately re-run onboarding.
RUN CLAUDE_VERSION="$(claude --version 2>/dev/null | awk '{print $1}')" \
    && jq -n --arg v "$CLAUDE_VERSION" '{ \
         hasCompletedOnboarding: true, \
         lastOnboardingVersion: $v, \
         projects: { \
           "/sandbox": {hasTrustDialogAccepted: true, projectOnboardingSeenCount: 1}, \
           "/sandbox/repo": {hasTrustDialogAccepted: true, projectOnboardingSeenCount: 1} \
         } \
       }' > /sandbox/.claude.json \
    && chown sandbox:sandbox /sandbox/.claude.json

# Status reporting, so the list view can say "this agent needs you" rather than
# only "this sandbox is up".
#
# The reporter is a script on PATH rather than seven copies of a shell one-liner
# inlined into settings.json: the payload handling lives in one place, and both
# files can be linted and tested outside the image.
#
# `Notification` is the signal the whole tool exists to deliver -- Claude Code
# fires it when it needs permission for a tool, or when it has been waiting on
# input. `Stop` ends a turn, so it means idle rather than finished for good.
# PreToolUse and PostToolUse mostly keep the timestamp fresh, which is what
# distinguishes an agent still working from a stale file left by a crashed one.
COPY sbx-status /usr/local/bin/sbx-status
COPY claude-settings.json /sandbox/.claude/settings.json

RUN chmod 0755 /usr/local/bin/sbx-status \
    && chmod 0644 /sandbox/.claude/settings.json \
    && chown -R sandbox:sandbox /sandbox/.claude

USER sandbox

# Inherited from the base image, restated so a future base change is visible
# here rather than silently altering behaviour.
ENTRYPOINT ["/bin/bash"]
