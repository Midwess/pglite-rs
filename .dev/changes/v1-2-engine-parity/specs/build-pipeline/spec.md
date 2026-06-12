# Delta for Build Pipeline

## ADDED Requirements

### Requirement: Per-extension prebuilt artifacts

The build pipeline SHALL produce one artifact per supported extension per target, ABI-matched to the engine by sharing its release tag, named `pglite-ext-<name>-<target>.tar.gz`, containing the loadable module under `lib/postgresql/` and control/SQL files under `share/postgresql/extension/`.

#### Scenario: Building pgcrypto

- WHEN `native/build-extensions.sh pgcrypto` runs with system OpenSSL available and a built engine install tree
- THEN it emits `pglite-ext-pgcrypto-<target>.tar.gz` (+ `.sha256`) with the dylib, `.control`, and versioned SQL files at relative runtime paths

#### Scenario: pgvector submodule uninitialized

- WHEN the script targets pgvector and `pglite/other_extensions/vector` is not checked out
- THEN it initializes the submodule before invoking PGXS

### Requirement: Cargo-feature-gated extension merge

`crates/pglite/build.rs` SHALL, for each enabled extension feature, resolve the matching extension artifact (env override → local build dir → cache → release download with sha256 verification) and merge its contents into the single `OUT_DIR/pglite-runtime.tar`.

#### Scenario: No extension features

- WHEN no extension feature is enabled
- THEN the base runtime tar is copied unchanged with no downloads and no re-tar

#### Scenario: pgvector feature enabled

- WHEN built with `--features pgvector`
- THEN the embedded runtime tar contains `vector` module + control/SQL files AND `CREATE EXTENSION vector` succeeds at runtime

### Requirement: ICU engine variant

The `icu` cargo feature SHALL cause `pglite-sys/build.rs` to link the `pglite-icu-<target>.tar.gz` engine variant (built `--with-icu`) from an isolated cache directory, and CI SHALL publish that variant for every supported target on the same engine tag.

#### Scenario: ICU build

- WHEN built with `--features icu`
- THEN the linked `libpglite.a` is the ICU variant and ICU collations are available at runtime

#### Scenario: Variant cache isolation

- WHEN a host builds both with and without `icu`
- THEN the two `libpglite.a` files never overwrite each other
