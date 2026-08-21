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

# A long scrollback is what makes capture-pane useful for previews and, later,
# for detecting what the agent is waiting on.
RUN printf '%s\n' \
        'set -g history-limit 50000' \
        'set -g status off' \
        'set -g mouse on' \
        'set -g escape-time 10' \
    > /etc/tmux.conf \
    && chmod 0644 /etc/tmux.conf

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

USER sandbox

# Inherited from the base image, restated so a future base change is visible
# here rather than silently altering behaviour.
ENTRYPOINT ["/bin/bash"]
