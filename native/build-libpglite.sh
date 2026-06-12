#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT/postgres-pglite"
OUT="$ROOT/native/out"
BUILD="$OUT/build"
PREFIX="$OUT/install"
CC="${CC:-clang}"

mkdir -p "$BUILD" "$PREFIX"

PGLITEC_COMPAT="-include stdlib.h -include stdbool.h"
if [ "$(uname)" = "Darwin" ]; then
  PGLITEC_COMPAT="$PGLITEC_COMPAT -D__key=_key"
fi

"$CC" -O2 -fPIC $PGLITEC_COMPAT -c "$SRC/pglite/src/pglitec/pglitec.c" -o "$OUT/pglitec.o"

if [ ! -f "$BUILD/config.status" ]; then
  (cd "$BUILD" && "$SRC/configure" \
    --prefix="$PREFIX" \
    --without-icu \
    --without-readline \
    --with-zlib \
    CFLAGS="-O2 -fPIC")
fi
