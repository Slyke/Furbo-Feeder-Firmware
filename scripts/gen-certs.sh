#!/bin/sh
set -eu
# Usage: gen-certs.sh OUTPUT_DIR DNS_NAME [IP ...]
out=$1
name=$2
shift 2
umask 077
mkdir -p "$out"
test ! -e "$out/server.key" || { echo "Refusing to overwrite an existing key" >&2; exit 1; }
san="DNS:$name"
for addr in "$@"; do
    case "$addr" in *[!0-9.:a-fA-F]*) echo "Invalid IP address" >&2; exit 1;; esac
    san="$san,IP:$addr"
done
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -sha256 -nodes -days 3650 \
    -subj "/CN=$name" -addext "subjectAltName=$san" \
    -keyout "$out/server.key" -out "$out/server.crt"
chmod 600 "$out/server.key"
echo "Trust server.crt on clients; keep server.key private."
