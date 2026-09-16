#!/bin/sh
set -eu
printf '%s\n' "$DARKWIRE_NFT_RULES" | nft -f -
if [ "$#" -eq 0 ]; then
  touch /tmp/ready
  exec su-exec 65532:65532 sleep infinity
fi
exec su-exec 65532:65532 /usr/local/bin/darkwire-environment proxy "$@"
