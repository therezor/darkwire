#!/bin/sh
# Install the filter, then drop to the proxy's own uid and stay there.
#
# There is no no-argument branch any more: every allow-list reaches the proxy,
# because the proxy is the only thing that resolves a name, so the gateway is
# never started without one. The proxy writes /tmp/ready itself once it is
# listening, which is what the engine waits on.
set -eu
printf '%s\n' "$DARKWIRE_NFT_RULES" | nft -f -
exec su-exec 65532:65532 /usr/local/bin/darkwire-environment proxy "$@"
