#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT/postgres-pglite"
OUT="$ROOT/native/out"
PREFIX="$OUT/install"
PG_CONFIG="$PREFIX/bin/pg_config"
TARGET="${TARGET:-$(rustc -vV 2>/dev/null | sed -n 's/host: //p')}"

if [ "$(uname)" = "Darwin" ] && command -v brew >/dev/null 2>&1; then
  OPENSSL_PREFIX="$(brew --prefix openssl@3 2>/dev/null || brew --prefix openssl)"
  export PKG_CONFIG_PATH="$OPENSSL_PREFIX/lib/pkgconfig:${PKG_CONFIG_PATH:-}"
  EXT_CPPFLAGS="-I$OPENSSL_PREFIX/include"
  EXT_LDFLAGS="-L$OPENSSL_PREFIX/lib"
else
  EXT_CPPFLAGS=""
  EXT_LDFLAGS=""
fi

build_ext() {
  name="$1"
  dir="$2"
  staging="$OUT/ext-staging-$name"
  rm -rf "$staging"
  mkdir -p "$staging"

  extra_ldflags_sl=""
  if [ "$name" = "pgcrypto" ]; then
    extra_ldflags_sl="$EXT_LDFLAGS -lcrypto"
  fi

  make -C "$dir" clean >/dev/null 2>&1 || true
  make -C "$dir" USE_PGXS=1 PG_CONFIG="$PG_CONFIG" \
    PG_CPPFLAGS="$EXT_CPPFLAGS" LDFLAGS_SL="$extra_ldflags_sl" -j
  make -C "$dir" USE_PGXS=1 PG_CONFIG="$PG_CONFIG" \
    PG_CPPFLAGS="$EXT_CPPFLAGS" LDFLAGS_SL="$extra_ldflags_sl" \
    DESTDIR="$staging" install

  (cd "$staging$PREFIX" && tar czf "$OUT/pglite-ext-$name-$TARGET.tar.gz" .)
  shasum -a 256 "$OUT/pglite-ext-$name-$TARGET.tar.gz" | awk '{print $1}' \
    > "$OUT/pglite-ext-$name-$TARGET.tar.gz.sha256"
  rm -rf "$staging"
  ls -lh "$OUT/pglite-ext-$name-$TARGET.tar.gz"
}

for name in "$@"; do
  case "$name" in
    pgcrypto)
      build_ext pgcrypto "$SRC/contrib/pgcrypto"
      ;;
    pgvector)
      git -C "$SRC" submodule update --init pglite/other_extensions/vector
      build_ext pgvector "$SRC/pglite/other_extensions/vector"
      ;;
    *)
      echo "unknown extension: $name" >&2
      exit 1
      ;;
  esac
done
