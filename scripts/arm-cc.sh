#!/bin/sh
# ring is freestanding; clang's built-in headers suffice for its ARM C/ASM.
exec clang --target=armv7-linux-musleabihf -march=armv7-a -mfpu=neon -mfloat-abi=hard -ffreestanding "$@"
