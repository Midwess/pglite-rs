#!/bin/bash
# Make the runtime payload portable: bundle libpq and rewrite the engine
# binaries' references to it relocatably, then regenerate pglite-runtime.tar and
# verify it by running initdb from a fresh extract. Runs after build-libpglite.sh,
# per platform. Kept out of build-libpglite.sh so the engine build cache stays
# stable. Idempotent.
set -euo pipefail

OUT="${1:-native/out}"
PREFIX="$OUT/install"
EXE=""
LIBS=""

case "$(uname)" in
  MINGW*|MSYS*)
    EXE=".exe"
    for f in "$PREFIX/bin/initdb.exe" "$PREFIX/bin/postgres.exe" "$PREFIX/lib/postgresql/libpqwalreceiver.dll"; do
      [ -f "$f" ] || continue
      deps="$(ldd "$f" 2>/dev/null | awk '{print $3}' | grep -iE '/(ucrt64|mingw64|clang64)/' || true)"
      for dll in $deps; do [ -f "$dll" ] && cp -n "$dll" "$PREFIX/bin/"; done
    done
    LIBS="$(cd "$PREFIX" && ls bin/*.dll | tr '\n' ' ')"
    ;;
  Darwin)
    install_name_tool -id "@rpath/libpq.5.dylib" "$PREFIX/lib/libpq.5.dylib"
    for spec in "bin/initdb:@loader_path/../lib/libpq.5.dylib" \
                "lib/postgresql/libpqwalreceiver.dylib:@loader_path/../libpq.5.dylib"; do
      f="${spec%%:*}"; new="${spec##*:}"
      cur="$(otool -L "$PREFIX/$f" | awk '/libpq\.5\.dylib/{print $1; exit}')"
      [ -n "$cur" ] && install_name_tool -change "$cur" "$new" "$PREFIX/$f"
    done
    codesign -f -s - "$PREFIX/lib/libpq.5.dylib" "$PREFIX/bin/initdb" \
      "$PREFIX/lib/postgresql/libpqwalreceiver.dylib"
    LIBS="lib/libpq.5.dylib"
    ;;
  Linux)
    patchelf --set-rpath '$ORIGIN/../lib:$ORIGIN/..' \
      "$PREFIX/bin/initdb" "$PREFIX/bin/postgres" "$PREFIX/lib/postgresql/libpqwalreceiver.so"
    LIBS="$(cd "$PREFIX" && ls lib/libpq.so* | tr '\n' ' ')"
    ;;
esac

tar -C "$PREFIX" --exclude lib/postgresql/pgxs -cf "$OUT/pglite-runtime.tar" \
  share/postgresql lib/postgresql "bin/initdb$EXE" "bin/postgres$EXE" $LIBS

V="$(mktemp -d)"
tar -C "$V" -xf "$OUT/pglite-runtime.tar"
"$V/bin/initdb$EXE" --version
echo "relocate-runtime: $OUT/pglite-runtime.tar verified portable"
