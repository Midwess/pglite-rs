#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT/postgres-pglite"
OUT="$ROOT/native/out"
BUILD="$OUT/build"
PREFIX="$OUT/install"
CC="${CC:-clang}"

mkdir -p "$BUILD" "$PREFIX"

for p in "$ROOT"/native/patches/*.patch; do
  if git -C "$SRC" apply --check "$p" 2>/dev/null; then
    git -C "$SRC" apply "$p"
  fi
done

PGLITEC_COMPAT="-include stdlib.h -include stdbool.h"
if [ "$(uname)" = "Darwin" ]; then
  PGLITEC_COMPAT="$PGLITEC_COMPAT -D__key=_key"
fi

"$CC" -O2 -fPIC $PGLITEC_COMPAT -Dexit=pgl_native_exit -c "$SRC/pglite/src/pglitec/pglitec.c" -o "$OUT/pglitec.o"
"$CC" -O2 -fPIC -I"$ROOT/native" -c "$ROOT/native/pglite_native.c" -o "$OUT/pglite_native.o"

if [ ! -f "$BUILD/config.status" ]; then
  (cd "$BUILD" && "$SRC/configure" \
    --prefix="$PREFIX" \
    --without-icu \
    --without-readline \
    --with-zlib \
    CFLAGS="-O2 -fPIC")
fi

PGLITE_DEFS="-D__PGLITE__ \
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

SL_FLAGS=""
if [ "$(uname)" = "Darwin" ]; then
  SL_FLAGS="-Wl,-undefined,dynamic_lookup"
fi

LINK_OBJS="$OUT/pglitec.o $OUT/pglite_native.o"

rm -f "$BUILD/src/backend/main/main.o" "$BUILD/src/backend/main/objfiles.txt" "$BUILD/src/bin/initdb/initdb.o"
make -C "$BUILD" -j"$NPROC" COPT="$PGLITE_DEFS" LDFLAGS_EX="$LINK_OBJS" LDFLAGS_SL="$SL_FLAGS"
make -C "$BUILD" install COPT="$PGLITE_DEFS" LDFLAGS_EX="$LINK_OBJS" LDFLAGS_SL="$SL_FLAGS"

rm -f "$BUILD/src/backend/main/main.o"
make -C "$BUILD/src/backend/main" main.o COPT="$PGLITE_DEFS -Dmain=pgl_backend_main"

rm -f "$BUILD/src/bin/initdb/initdb.o"
make -C "$BUILD/src/bin/initdb" initdb.o COPT="$PGLITE_DEFS -Dmain=pgl_initdb_main"

(cd "$BUILD" && ld -r -o "$OUT/initdb_bundle.o" \
  src/bin/initdb/initdb.o src/bin/initdb/findtimezone.o src/bin/initdb/localtime.o \
  src/fe_utils/libpgfeutils.a src/common/libpgcommon.a src/port/libpgport.a)

printf '_pgl_initdb_main\n' > "$OUT/initdb_keep.txt"
if [ "$(uname)" = "Darwin" ]; then
  nmedit -s "$OUT/initdb_keep.txt" "$OUT/initdb_bundle.o"
else
  objcopy --keep-global-symbol=pgl_initdb_main "$OUT/initdb_bundle.o"
fi

BACKEND_OBJS="$(cd "$BUILD" && cat $(find src/backend src/timezone -name objfiles.txt) | tr ' ' '\n' | sed '/^$/d' | sort -u)"

if [ "$(uname)" = "Darwin" ]; then
  (cd "$BUILD" && libtool -static -o "$OUT/libpglite.a" $BACKEND_OBJS \
    src/common/libpgcommon_srv.a src/port/libpgport_srv.a \
    "$OUT/pglitec.o" "$OUT/pglite_native.o" "$OUT/initdb_bundle.o")
else
  (cd "$BUILD" && {
    echo "create $OUT/libpglite.a"
    for o in $BACKEND_OBJS; do echo "addmod $o"; done
    echo "addlib src/common/libpgcommon_srv.a"
    echo "addlib src/port/libpgport_srv.a"
    echo "addmod $OUT/pglitec.o"; echo "addmod $OUT/pglite_native.o"
    echo "addmod $OUT/initdb_bundle.o"
    echo "save"
    echo "end"
  } | ar -M)
fi

tar -C "$PREFIX" --exclude lib/postgresql/pgxs -cf "$OUT/pglite-runtime.tar" share/postgresql lib/postgresql bin/initdb bin/postgres

ls -lh "$OUT/libpglite.a" "$OUT/pglite-runtime.tar"
