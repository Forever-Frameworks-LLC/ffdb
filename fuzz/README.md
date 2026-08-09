# FFDB fuzz targets

The fuzz package exercises the untrusted SQL boundary with libFuzzer and is kept
outside the main Cargo workspace so sanitizer and nightly-only build flags do not
affect production crates.

## Local use

Install a nightly toolchain and the CI-pinned runner, then run commands from the
repository root:

```sh
rustup toolchain install nightly
cargo install cargo-fuzz --version 0.13.2 --locked
mkdir -p fuzz/corpus/sql_parser fuzz/corpus/rls_bypass
cp fuzz/seeds/sql_parser/*.sql fuzz/corpus/sql_parser/
cp fuzz/seeds/rls_bypass/*.sql fuzz/corpus/rls_bypass/
cargo +nightly fuzz run --fuzz-dir fuzz sql_parser fuzz/corpus/sql_parser -- \
  -dict=fuzz/sql.dict -max_len=4096
cargo +nightly fuzz run --fuzz-dir fuzz rls_bypass fuzz/corpus/rls_bypass -- \
  -dict=fuzz/sql.dict -max_len=2048 -timeout=5
```

Use `-max_total_time=60` for a bounded smoke run. A saved failure can be replayed
by passing its artifact path after the target name:

```sh
cargo +nightly fuzz run --fuzz-dir fuzz rls_bypass \
  fuzz/artifacts/rls_bypass/CRASH_FILE
```

## Invariant scope

`sql_parser` feeds arbitrary valid UTF-8 to statement splitting, single-statement
classification, and multi-statement RLS parsing. All three must remain total and
deterministic; crashes, hangs, and inconsistent results are failures.

`rls_bypass` creates a fresh RLS-protected SQLite database for every input. For
SELECT statements, Bob's row-bearing result or error must be identical with and
without Alice's protected canary row. Connection-history metadata such as
`affected_rows` is normalized before this comparison. This covers direct values,
projections, and aggregates without mistaking a caller-supplied literal for a
leak. After every statement, Alice's exact row must still exist unchanged. The
harness therefore covers direct disclosure and unauthorized update/delete through
parser, authorizer, view, and generated-trigger interactions. It does not model
timing, resource-use, unique-constraint, or other side channels, and it is not a
substitute for the deterministic RLS and SQL compatibility tests in the main
workspace.

Keep small semantic seeds and `sql.dict` in source control. Generated coverage
corpora and crash artifacts remain ignored; minimize and promote useful
regressions into deterministic tests before fixing them.
