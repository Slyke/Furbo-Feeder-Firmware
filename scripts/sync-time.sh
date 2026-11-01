#!/bin/sh
set -eu
# The SSH destination should be configured with a pinned host key.
# Example: scripts/sync-time.sh furbo-local
exec ssh "$1" "date -u -s @$(date -u +%s)"
