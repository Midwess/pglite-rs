#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT/postgres-pglite"
OUT="$ROOT/native/out"
CC="${CC:-clang}"

ICU_CONFIGURE="--without-icu"
ICU_ARCHIVES=""
if [ "${WITH_ICU:-}" = "1" ]; then
  OUT="$OUT/icu"
  ICU_CONFIGURE="--with-icu"
  if [ "$(uname)" = "Darwin" ] && command -v brew >/dev/null 2>&1; then
    export PKG_CONFIG_PATH="$(brew --prefix icu4c)/lib/pkgconfig:${PKG_CONFIG_PATH:-}"
  fi
  ICU_LIBDIR="$(pkg-config --variable=libdir icu-uc)"
  ICU_ARCHIVES="$ICU_LIBDIR/libicui18n.a $ICU_LIBDIR/libicuuc.a $ICU_LIBDIR/libicudata.a"
fi

BUILD="$OUT/build"
PREFIX="$OUT/install"

mkdir -p "$BUILD" "$PREFIX"

for p in "$ROOT"/native/patches/*.patch; do
  if git -C "$SRC" apply --check "$p" 2>/dev/null; then
    git -C "$SRC" apply "$p"
  elif git -C "$SRC" apply --check -R "$p" 2>/dev/null; then
    echo "patch already applied: $p"
  elif [ -n "${GITHUB_ACTIONS:-}" ]; then
    echo "FATAL: patch does not apply: $p" >&2
    exit 1
  else
    echo "WARNING: patch does not apply cleanly (assuming already applied): $p" >&2
  fi
done

PGLITEC_COMPAT="-include stdlib.h -include stdbool.h"
if [ "$(uname)" = "Darwin" ]; then
  PGLITEC_COMPAT="$PGLITEC_COMPAT -D__key=_key"
fi

"$CC" -O2 -fPIC $PGLITEC_COMPAT -Dexit=pgl_native_exit -c "$SRC/pglite/src/pglitec/pglitec.c" -o "$OUT/pglitec.o"
"$CC" -O2 -fPIC -I"$ROOT/native" -c "$ROOT/native/pglite_native.c" -o "$OUT/pglite_native.o"
"$CC" -O2 -fPIC -I"$ROOT/native" -c "$ROOT/native/pglite_reset.c" -o "$OUT/pglite_reset.o"
"$CC" -O2 -fPIC -I"$ROOT/native" -c "$ROOT/native/pglite_static_ext.c" -o "$OUT/pglite_static_ext.o"

if [ ! -f "$BUILD/config.status" ]; then
  (cd "$BUILD" && "$SRC/configure" \
    --prefix="$PREFIX" \
    "$ICU_CONFIGURE" \
    --without-readline \
    --with-zlib \
    CFLAGS="-O2 -fPIC")
fi

PGLITE_DEFS="-D__PGLITE__ -U_FORTIFY_SOURCE \
-Dsystem=pgl_system -Dpopen=pgl_popen -Dpclose=pgl_pclose \
-Dgeteuid=pgl_geteuid -Dgetuid=pgl_getuid -Dgetpwuid=pgl_getpwuid \
-Dexit=pgl_exit \
-Dmunmap=pgl_munmap \
-Dfcntl=pgl_fcntl \
-Datexit=pgl_atexit \
-Dsetsockopt=pgl_setsockopt -Dgetsockopt=pgl_getsockopt -Dgetsockname=pgl_getsockname \
-Drecv=pgl_recv -Dsend=pgl_send -Dconnect=pgl_connect \
-Dpoll=pgl_poll \
-Dshmget=pgl_shmget -Dshmat=pgl_shmat -Dshmdt=pgl_shmdt -Dshmctl=pgl_shmctl \
-Dlongjmp=pgl_longjmp -Dsiglongjmp=pgl_siglongjmp \
-Dsetitimer=pgl_native_setitimer"

NPROC="$( (command -v nproc >/dev/null 2>&1 && nproc) || sysctl -n hw.ncpu )"

LINK_OBJS="$OUT/pglitec.o $OUT/pglite_native.o"

SL_FLAGS=""
if [ "$(uname)" = "Darwin" ]; then
  SL_FLAGS="-Wl,-undefined,dynamic_lookup"
fi
case "$(uname)" in MINGW*|MSYS*) SL_FLAGS="$LINK_OBJS -lws2_32" ;; esac

rm -f "$BUILD/src/backend/main/main.o" "$BUILD/src/backend/main/objfiles.txt" "$BUILD/src/bin/initdb/initdb.o" "$BUILD/src/backend/postgres"
make -C "$BUILD" -j"$NPROC" COPT="$PGLITE_DEFS" LDFLAGS_EX="$LINK_OBJS" LDFLAGS_SL="$SL_FLAGS"
make -C "$BUILD" install COPT="$PGLITE_DEFS" LDFLAGS_EX="$LINK_OBJS" LDFLAGS_SL="$SL_FLAGS"

rm -f "$BUILD/src/backend/main/main.o"
make -C "$BUILD/src/backend/main" main.o COPT="$PGLITE_DEFS -Dmain=pgl_backend_main"

BACKEND_OBJS="$(cd "$BUILD" && cat $(find src/backend src/timezone -name objfiles.txt) | tr ' ' '\n' | sed '/^$/d' | sort -u)"

if [ "$(uname)" = "Darwin" ]; then
  (cd "$BUILD" && libtool -static -o "$OUT/libpglite.a" $BACKEND_OBJS \
    src/common/libpgcommon_srv.a src/port/libpgport_srv.a \
    "$OUT/pglitec.o" "$OUT/pglite_native.o" "$OUT/pglite_reset.o" \
    "$OUT/pglite_static_ext.o" $ICU_ARCHIVES)
else
  (cd "$BUILD" && {
    echo "create libpglite.a"
    for o in $BACKEND_OBJS; do echo "addmod $o"; done
    echo "addlib src/common/libpgcommon_srv.a"
    echo "addlib src/port/libpgport_srv.a"
    case "$(uname)" in MINGW*|MSYS*) echo "delete getopt_srv.o getopt_long_srv.o" ;; esac
    echo "addmod ../pglitec.o"; echo "addmod ../pglite_native.o"; echo "addmod ../pglite_reset.o"
    echo "addmod ../pglite_static_ext.o"
    case "$(uname)" in MINGW*|MSYS*)
      for o in src/pl/plpgsql/src/*.o; do echo "addmod $o"; done ;;
    esac
    for a in $ICU_ARCHIVES; do echo "addlib $a"; done
    echo "save"
    echo "end"
  } | ar -M && mv -f libpglite.a "$OUT/libpglite.a")
fi

EXE=""
case "$(uname)" in MINGW*|MSYS*) EXE=".exe" ;; esac
tar -C "$PREFIX" --exclude lib/postgresql/pgxs -cf "$OUT/pglite-runtime.tar" share/postgresql lib/postgresql "bin/initdb$EXE" "bin/postgres$EXE"

ls -lh "$OUT/libpglite.a" "$OUT/pglite-runtime.tar"
