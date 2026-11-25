# Repository Guidelines

Resumable mini JavaScript VM: a strict-mode JS subset compiles to explicit-slot bytecode, runs on an async VM, and snapshots/restores at `await` without serializing a native call stack. This repository is the first vertical slice of `docs/design.md`, not the full engine.

Keep this file an index plus binding rules. Mechanisms, invariants, and per-file guidance belong in the documents below, not here.

## Documentation index

| File | Content |
| --- | --- |
| `docs/design.md` | Target design overview and topic index, Chinese. Not a status report. |
| `docs/design/` | Topic designs: build inputs, language subset, framework acceptance, WinterTC/Fetch, host interface, host bindings, bytecode, snapshot, risks. |
| `docs/design/host-bindings.md` | Host binding contract: declarations, `JsValue`, operations, snapshot records. |
| `docs/implemented.md` | What the current JS subset implements, derived from code. |
| `docs/gaps.md` | What is rejected or missing today, derived from code. |
| `docs/internals.md` | Crate pipeline, public entry points, suspend/resume and snapshot invariants, change-location table, easy-to-break areas, code conventions. |
| `docs/testing.md` | Test layers, `// @` case contract, case categories, host declaration fixtures. |

`docs/` 根目录只放持久文档（设计与当前契约）。临时性文档（性能基线、优化计划与对照数据）放在 `docs/perf/`，原始 CSV 数据留在其对应的子目录中。

## Rules

- Do not implement or assume a design feature the code lacks.
- Unsupported syntax must fail at compile time as `unsupported syntax: …` with source line and column.
- Do not weaken the suspend/resume, snapshot, or host-binding invariants in `docs/internals.md`. Incompatible bytecode changes bump `BYTECODE_VERSION`; snapshot record-shape changes bump `SNAPSHOT_FORMAT_VERSION`; build artifact container or encoding changes bump `BUILD_FORMAT_VERSION`. Bump each contract only when it changed.
- Observable JS behavior is a `.js` case, not a new Rust harness. Snapshot identity, heap identity, slot tables, and rejected restores belong in `crates/harness/tests/e2e.rs` / `e2e/`. Keep every fixture in a category directory of `crates/harness/tests/cases/`, never at its top level.
- `README.md` stays overview-level: no slot/snapshot mechanics, format versions, opcodes, builtin lists, or per-feature behavior notes.
- After a behavior change, refresh `docs/gaps.md` and `docs/implemented.md` in the same commit; update `README.md` and the affected file under `docs/design.md` / `docs/design/` when the target design is affected.
- Rust only. No npm, Node, or package manager for the engine; JS files are fixtures compiled by this compiler.
- Edition 2024, rustfmt and clippy defaults. No `rust-toolchain.toml`, `rustfmt.toml`, `clippy.toml`, `[lints]`, `[features]`, or `.cargo/config.toml`.
- Add a dependency only if the bytecode/VM/host boundary needs it. Oxc stays pinned and must not leak past `CompileError`.
- Small public surface: crate internals are `pub(crate)`. Module docs (`//!`) state invariants; code comments are English; README and `docs/` are Chinese.
- License `MIT OR Apache-2.0`, version `0.1.0`.

## Commands

```sh
cargo test --workspace
cargo run -p yield-harness          # hardcoded two-await demo; expects 15; no CLI flags
cargo test -p yield-compiler --test compile
cargo test -p yield-harness --test e2e
cargo test -p yield-harness --test cases
cargo test -p yield-harness case_fulfilled_await_resumes_after_restore
cargo build --release
cargo fmt --check
cargo clippy --workspace --all-targets
```

Package names are `yield-vm`, `yield-compiler`, `yield-harness`. There is no `scripts/` directory and no CI.

## Commit workflow

Commit immediately after each completed phase of behavior change (new syntax support, VM/snapshot contract, host protocol, new cases), and also after each completed refactor (file splits, renames, visibility changes that do not alter behavior); never accumulate one large commit. Before committing, in order: `cargo fmt --check`, `cargo clippy --workspace --all-targets`, `cargo test --workspace` (plus the matching layer in `docs/testing.md` for contract changes), then the doc sync above. Commit messages are a single imperative English sentence describing the behavior change, e.g. `snapshot: reject restore when natives are missing`; refactors use `refactor: …` describing the move or split. Docs and tests land in the same commit.

## Before editing

Read `docs/internals.md` for the pipeline, invariants, and the change-location table. Read `docs/testing.md` before adding or changing a case.
