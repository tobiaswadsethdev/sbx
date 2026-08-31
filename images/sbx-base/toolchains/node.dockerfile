# node -- already in the base image, so this layer installs nothing.
#
# The community base ships node 22 and npm at /usr/bin, and a toolchain here is
# not only an install: it is also the registry the policy has to open and the
# line in the manifest that lets `sbx doctor` say what a variant carries. Node
# needs the second and third of those and not the first, so the layer records
# what is already there instead of downloading it again.
#
# Asserted rather than assumed. If a future base image drops node -- or moves it
# off /usr/bin, which would silently break `npm_registry`'s binary rule, since
# the gateway matches the kernel-resolved interpreter and not /usr/bin/npm --
# this fails the build with the reason, rather than producing a `sbx-base:node`
# with no node in it.
RUN set -eu; \
    test -x /usr/bin/node || { echo "the base image no longer has /usr/bin/node" >&2; exit 1; }; \
    test -x /usr/bin/npm  || { echo "the base image no longer has /usr/bin/npm" >&2; exit 1; }; \
    printf 'node %s\n' "$(node --version | sed 's/^v//')" >> /usr/local/share/sbx/toolchains
