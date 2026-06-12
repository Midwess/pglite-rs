#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/native/out"
CC="${CC:-clang}"

"$CC" -O2 -I"$ROOT/native" -o "$OUT/smoke" "$ROOT/native/smoke.c" "$OUT/libpglite.a" -lz -lm -pthread
