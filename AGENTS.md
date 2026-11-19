# Repository Guidelines

Resumable mini JavaScript VM. A strict-mode JS subset compiles to explicit-slot bytecode, runs on an async VM, and can snapshot/restore at `await` without serializing a native call stack. This repo is the first vertical slice, not the full engine in `docs/design.md`.

`docs/design.md` is the target (Solid/React acceptance, Proxy, C ABI, tracing GC). `README.md` is what exists. Do not implement or assume a design feature unless the code already has it. Unsupported syntax must fail at compile time as `unsupported syntax: …` with source line/column.

## Architecture & Data Flow

Workspace crates, one direction of dependency:

```text
yield-poc  →  yield-compiler  →  yield-vm
```

`yield-vm` owns the bytecode contract and never imports the compiler. Oxc AST stays inside `yield-compiler` (`frontend` and `lower` only). The only Oxc type on the public compiler surface is `oxc::span::Span` on `CompileError`.

```text
source (parsed as .mjs only to force strict mode; import/export rejected)
  → frontend     parse, top-level function declarations
  → lower        bindings, exceptions, await  →  LoweredFunction
  → bindings     which ids stay shared across functions
  → promote      unshared bindings become logical registers
  → analyze      CFG live-in, including both resume edges and handlers
  → allocate     persistent slots for cross-await values, transient otherwise
  → Build        versioned bytecode; build_id = digest of execution-relevant fields
  → Vm           validate, run, host operations, snapshot / restore
```

Public entry points:

- `yield_compiler::compile(source, &CompileOptions { imports }) -> Result<Build, CompileError>`
- `Vm::new(build, &host)` checks imports before any script runs
- `Vm::start(host, "run", args)` then `Vm::run_until_idle(host)`
- `Vm::snapshot(&[roots])` only when the microtask queue is empty
- `Vm::restore(snapshot, same_build, host)` rebinds natives and operations before execution
- Host async work is `HostReply::Operation { operation_id }`, later `Vm::complete_operation`

Suspend/resume invariants (do not weaken these):

- Operands are explicit slots. Nothing implicit is persisted across `Instr::Await`.
- `ResumeEntry` is `{ fulfill_pc, fulfill_dst, reject_pc, reject_dst, saved_slots }`. Continuations are keyed by `resume_id`, not a runtime PC.
- `saved_slots` is the union of both resume entries' live-in after subtracting that entry's result register. Result slots are written on resume, not saved. Persistent slots are not reused across different live values. Transient slots are dropped on suspend.
- `undefined` is a real value. Frame slot `None` means uninitialized, not `undefined`.
- Bindings read or written from another function stay in a heap environment. Unshared bindings are frame slots. A call that shares nothing and creates no closure allocates no environment.
- `finally` is explicit completion state (kind + payload), not a native exception stack. Nested `await` inside `finally` must keep that pending completion.
- Settled promises still resume through the microtask queue. `AWAIT` never continues inline.
- Snapshot roots: pending operations, async continuations, closure environments, plus caller-supplied script roots. Native pointers, host callbacks, and queued microtasks are not in the snapshot. Native objects are `(type, id)` keys.
- Restore is exact-version: `SNAPSHOT_FORMAT_VERSION`, `BYTECODE_VERSION`, and `build_id` must match. Missing native binding or missing operation rejects the whole restore; the VM must not start. No snapshot migration.

`Host::call` is synchronous and must not re-enter the VM. The host is authoritative for operation state; the VM stores only the operation id and its Promise link.

## Key Directories

| Path | Role |
| --- | --- |
| `crates/vm/src` | Bytecode contract, loader checks, interpreter, heap/object table, Promise/microtasks, host protocol, snapshot |
| `crates/compiler/src` | Oxc frontend, lowering, liveness, slot allocation |
| `crates/compiler/src/lower` | Only body walker: `stmt`, `expr`, `control` |
| `crates/poc/src` | `TestHost`, directive case runner, demo binary |
| `crates/poc/tests/e2e.rs` | Hand-written snapshot/restore and loader tests |
| `crates/poc/tests/cases` | Semantic `.js` fixtures; one generated `#[test]` each |
| `docs/design.md` | Target design, Chinese. Not a status report |
| `docs/gaps.md` | Current JS-subset implementation gap inventory (what is rejected/missing today, derived from code). Status report; refresh after behavior changes. |

There is no `scripts/` directory and no CI.

## Development Commands

```sh
cargo test --workspace
cargo run -p yield-poc          # hardcoded two-await demo; expects 15; no CLI flags
cargo test -p yield-compiler --test compile
cargo test -p yield-poc --test e2e
cargo test -p yield-poc --test cases
cargo test -p yield-poc case_fulfilled_await_resumes_after_restore
cargo build --release
cargo fmt --check
cargo clippy --workspace --all-targets
```

Package names are `yield-vm`, `yield-compiler`, `yield-poc` (not the directory names). The demo binary has no arguments.

## Code Conventions & Common Patterns

- Edition 2024. Module docs (`//!`) state invariants; match that voice when adding a module. Code comments are English. README and `docs/design.md` are Chinese.
- Small public surface. Crate internals are `pub(crate)`. New opcodes go in `yield_vm::bytecode::Instr`, then `compiler/src/instr.rs` (uses/defs/throws/rewrite) and the lowerer. CFG and slot allocation must not match on instruction shape.
- Incompatible bytecode changes bump `BYTECODE_VERSION` (`crates/vm/src/bytecode.rs`). Snapshot record-shape changes bump `SNAPSHOT_FORMAT_VERSION` (`crates/vm/src/snapshot.rs`). Both are currently `5`. Do not bump them together unless both contracts changed. `Build::finalize` sets `build_id` from `compute_id`; content changes already reject old snapshots.
- Errors: `CompileError` is a located struct (`message`, `line`, `column`, `span`), not a `thiserror` enum. `VmError` is `thiserror` (`InvalidBuild`, `InvalidSnapshot`, `MissingImport` / `MissingNative` / `MissingOperation`, `Host`, `InvalidState`, `Thrown`). Script throws are `Value`s (`ExecOutcome::Thrown`), not `VmError`, until they reject an async result. Host failures are `String`.
- Ordered maps are `BTreeMap` / `BTreeSet` (imports, globals, operations, natives). Keep that when identity or snapshot order matters.
- `let` and `const` are not distinguished. Static TDZ for the current function lives in `compiler/src/env.rs` (`pending_lexicals`), not the emitter. A closure does not inherit that set, so an early cross-closure read is `undefined`. This is not full ECMAScript TDZ, and loop iterations do not get fresh bindings.
- Name policy (`compiler/src/names.rs`): locals shadow at value and call positions. Host imports and top-level functions are callable, not first-class values. `typeof` is the exception: an import or top-level function of the same name reports `"function"` even when a local shadows it. Preserve that until the policy is deliberately changed.
- Adding a host import used by fixtures means updating `IMPORTS` in `crates/poc/src/lib.rs` and `TestHost::call`. Demo host in `main.rs` is separate (`inspect`, `seed`, `wait` only).
- `TestHost` semantics: `seed()` → `10`; `wait(label)` → operation id `wait:{label}`; `bind(id)` → `NativeKey { type_name: "Node", id }`; `inspect` / `setLabel` → `undefined`. Failure injection: `fail_import`, `reject_native`, `reject_restore_operation`.

Where to edit:

| Change | Touch |
| --- | --- |
| Syntax subset | `compiler/src/lower` (worklist, not recursive borrow), `frontend.rs`; reject with `unsupported()` |
| Liveness / which slots survive await | `analyze.rs`, `allocate.rs` |
| New opcode | `vm/src/bytecode.rs`, `vm/src/exec.rs` (and `ops.rs` if it is a value op), `compiler/src/instr.rs`, lowerer |
| New heap kind | `vm/src/heap.rs` `write_edges`; snapshot reachability uses that list only |
| Snapshot or restore checks | `vm/src/snapshot.rs`; add a rejection test in `poc/tests/e2e.rs` |
| Host protocol | `vm/src/host.rs`, `value.rs` (`HostValue`); implementors: `poc/src/lib.rs`, `poc/src/main.rs` |
| Observable JS behavior | a `.js` case, not a new Rust harness |

Easy to break:

- Top level is function declarations plus empty statements only, at least one function. Nested functions are a worklist of Oxc nodes plus the captured scope chain.
- Before `allocate`, every `Slot` is a logical register stuffed into `SlotKind::Transient`. `slot.kind` is meaningless until `FrameLayout` remaps it. `promote` rewrites one instruction to one instruction so jump, resume, and handler PCs stay valid.
- `instr.rs::may_throw` is only `Throw`, `CallFunction`, and `CallValue`. A new throwing opcode must be listed there or exception edges never reach liveness or promotion.
- `Function::needs_environment` is derived from params and code (`GetBinding` and friends). Do not store a forgeable flag. Restore requires an env iff that function needs one.
- `Build::validate` rejects non-finite constants. `Vm` also rejects non-finite host numbers.
- IDs are counters (`ObjectId`, `ContinuationId`) with overflow checks. Snapshot JSON order follows `BTreeMap`.

## Important Files

- `Cargo.toml` — workspace members and pinned deps (`oxc = "=0.159.0"` with `ast_visit`)
- `crates/compiler/src/lib.rs` — pipeline and `compile`
- `crates/vm/src/lib.rs` — re-exports: `Build`, `Vm`, `Host`, `Snapshot`, `Value`, `VmError`
- `crates/vm/src/bytecode.rs` — `Instr`, `ResumeEntry`, `ExceptionHandler`, `Build::validate`
- `crates/vm/src/vm.rs` — `start`, `run_until_idle`, `complete_operation`
- `crates/vm/src/snapshot.rs` — `Vm::snapshot`, `Vm::restore`, JSON codec
- `crates/poc/src/case.rs` — `// @` directive runner
- `crates/poc/build.rs` — generates `OUT_DIR/cases.rs`
- `crates/poc/src/main.rs` — the only binary; embedded design-doc example

## Runtime/Tooling Preferences

- Rust only. No npm, Node, or package manager for the engine. JS files are fixtures, compiled by this compiler.
- No `rust-toolchain.toml`, `rustfmt.toml`, `clippy.toml`, `[lints]`, `[features]`, or `.cargo/config.toml`. Use rustfmt and clippy defaults.
- Edition 2024 implies Rust ≥ 1.85. MSRV is not otherwise declared.
- License `MIT OR Apache-2.0`, version `0.1.0`.
- Do not add a dependency unless the bytecode/VM/host boundary needs it. Parser stays Oxc, locked. Do not leak Oxc types past `CompileError`.

## Testing & QA

Three layers, all `cargo test`. No insta, no coverage gate, no `#[ignore]` / `#[should_panic]`, no unit-test modules inside `vm` or `compiler` sources. The VM is tested through `yield-poc`.

1. `crates/compiler/tests/compile.rs` — bytecode shape of the two-await example and unsupported-syntax location.
2. `crates/poc/tests/e2e.rs` — snapshot identity, loader rejection, finally-across-await, reachability. Assert `VmError` with `matches!`.
3. `crates/poc/tests/cases/*.js` — generated tests. `build.rs` requires a non-empty dir of `*.js` whose stems match `[a-z][a-z0-9_]*`. Test function is `case_<stem>`.

Case contract (`yield_poc::run_case`):

- Leading blank lines and `// @` directives are the header. The whole file, header included, is compiled so diagnostics keep file line numbers.
- Body must define `async function run`. The runner always starts `"run"`.
- `@args` JSON array, only as the first directive.
- `@pending ["wait:a"]`, `@restore` (JSON round-trip, then `Vm::restore`), `@fulfill ["wait:a", 2]`, `@reject ["wait:a", "boom"]`.
- Last directive must be `@returns`, `@fulfilled`, or `@rejected`. Scalars only: number, bool, string, `null`, `undefined`.
- Fixed compile imports: `inspect`, `seed`, `wait`, `bind`, `setLabel`.

Example:

```js
// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 2]
// @fulfilled 2

async function run() {
  return await wait("a");
}
```

Add a behavior case by dropping `snake_case.js` in `crates/poc/tests/cases/` (or one of its category subdirectories: `async`, `closures`, `control`, `functions`, `objects`, `promise`, `strings`). `build.rs` walks the directory recursively; the generated test is `case_<stem>` at top level or `case_<category>_<stem>` in a subdirectory. `cargo:rerun-if-changed=tests/cases` regenerates tests. Use `e2e.rs` when the assertion needs heap identity, slot tables, or a rejected restore.
