#!/bin/sh
for tool in llvm-ar llvm-ar-21 llvm-ar-20 llvm-ar-19 llvm-ar-18 llvm-ar-17 llvm-ar-16 llvm-ar-15 llvm-ar-14; do
    if command -v "$tool" >/dev/null 2>&1; then exec "$tool" "$@"; fi
done
echo "Install LLVM (llvm-ar) to cross-compile ring." >&2
exit 1
