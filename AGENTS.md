# Repository Guidelines

Resumable mini JavaScript VM. A strict-mode JS subset compiles to explicit-slot bytecode, runs on an async VM, and can snapshot/restore at `await` without serializing a native call stack. This repo is the first vertical slice, not the full engine in `docs/design.md`.

`docs/design.md` is the target (Solid/React acceptance, Proxy, C ABI, tracing GC). `README.md` is what exists. Do not implement or assume a design feature unless the code already has it. Unsupported syntax must fail at compile time as `unsupported syntax: …` with source line/column.

## Architecture & Data Flow

Workspace crates, one direction of dependency:

```text
yield-harness  →  yield-compiler  →  yield-vm
                       ↓
                  yield-host-dts   ←  yield-binding-macros  (expansion only)
```

`yield-vm` owns the bytecode contract and never imports the compiler. Oxc AST stays inside `yield-compiler` (`frontend` and `lower` only) and inside `yield-host-dts`; the latter exposes only plain name tables and byte spans. The only Oxc type on the public compiler surface is `oxc::span::Span` on `CompileError`.

```text
source roots and synchronous resolver (strict ES modules)
  → frontend     parse reachable modules and resolve import/export targets
  → lower        bindings, exceptions, await  →  LoweredFunction
  → bindings     which ids stay shared across functions
  → promote      unshared bindings become logical registers
  → analyze      CFG live-in, including both resume edges and handlers
  → allocate     persistent slots for cross-await values, transient otherwise
  → Build        versioned bytecode; build_id = digest of execution-relevant fields
  → Vm           validate, run, host operations, snapshot / restore
```

Public entry points:

- `yield_compiler::compile(entry, preload, resolver, &options) -> Result<Build, CompileError>` links explicit roots; `CompileOptions::from_dts(source)` reads the separate host import manifest from the host's `.d.ts`.
- `Vm::new(build, &mut host)` validates the build and imports, instantiates all source modules, evaluates the optional preload graph and installs its live global getters, then evaluates entry and drains initialization microtasks.
- `Vm::get_export("name")` reads the current value of an unambiguous linked entry export, including re-exports.
- `Vm::start(host, "run", args)` reads the public binding and calls it with an `undefined` receiver; `args` are `JsValue` constants, then the host calls `Vm::run_until_idle(host)`
- `Vm::snapshot(&[roots])` only when the microtask queue is empty
- `Vm::collect_garbage(roots)` explicitly sweeps unreachable heap state between calls, only with an empty microtask queue; `roots` is a slice of same-VM `Value`s the host still needs. It returns the number of heap objects removed and preserves live IDs.
- `Vm::restore(snapshot, same_build, host)` rebinds declarations and operations before execution
- `CallCx::promise(op)` starts external work with a native Promise target. Linked background tasks use the same registration path and have independently cancellable numeric handles. Deliver through `Vm::complete_operation(token, completion)` using the current VM generation token. Registrations release platform resources on drop. Publish the restored VM before calling `Vm::start_recovered_operations()`.

Suspend/resume invariants (do not weaken these):

- Operands are explicit slots. Nothing implicit is persisted across `Instr::Await` or `Instr::Yield`.
- `ResumeEntry` is `{ fulfill_pc, fulfill_dst, reject_pc, reject_dst, saved_slots }`. Continuations are keyed by `resume_id`, not a runtime PC.
- `saved_slots` is the union of both resume entries' live-in after subtracting that entry's result register. Result slots are written on resume, not saved. Persistent slots are not reused across different live values. Transient slots are dropped on suspend.
- A generator resumes through `GeneratorEntry { next_pc, next_dst, throw_pc, throw_dst, return_pc, saved_slots }`, indexed by `Instr::Yield::generator`. Subtract each entry's own injected register before joining the three live-ins; subtracting after the union drops a value another entry needs. `return_pc` has no result slot: it begins with `Instr::LoadGeneratorReturn`, so the pending completion payload lives in the rebuilt frame, never in VM-global state. An async generator's own `await` uses the ordinary `ResumeEntry` table with a `ContinuationTarget::Generator` continuation; the two tables are validated independently.
- `Instr::GeneratorStart` ends a generator call after parameter initialization: capture `Function::generator_start.saved_slots` and the invocation environment into a fresh generator object and return it without running the body. Only the start capture and the three yield entries are externally injected; ordinary control flow must not reach `throw_pc`, `return_pc`, or `body_pc`.
- Generator objects are ordinary objects plus a suspended frame (`func_id`, status, environment, values in entry order). An async generator's record additionally owns the FIFO request queue; a synchronous one never pushes there. `Executor` state is never snapshotted: capture records it and restore rejects it. Reentrant resume of a synchronous generator throws a script `TypeError`; on an async generator the request is appended instead. Consume generators only through `Symbol.iterator` and async generators only through `Symbol.asyncIterator`; do not special-case either in `for...of`.
- An async generator call allocates no result Promise: `GeneratorStart` returns the generator object. Each `next` / `throw` / `return` creates a request Promise, appends the request, and drives the queue one step at a time. The head is the in-progress request; a `yield` fulfils it with `done: false`, a body return or fall-through with `done: true`, and an uncaught script throw completes the generator and rejects it. `await` inside the body records the only continuation, marks the generator `Awaiting`, keeps the head request, and resumes the same body — never a fresh request. A completed generator settles `next` with `{value: undefined, done: true}`, rejects `throw`, and assimilates a `return` argument through a persisted `ReactionJob::GeneratorClose` before reporting `done: true`; later requests never pass the head.
- `undefined` is a real value. Frame slot `None` means uninitialized, not `undefined`.
- Bindings read or written from another function, plus exported bindings, stay in a heap environment. Other bindings are frame slots. A call that shares nothing and creates no closure allocates no environment.
- `finally` is explicit completion state (kind + payload), not a native exception stack. Nested `await` inside `finally` must keep that pending completion.
- After successful preparation, `Await` always resumes through the microtask queue, even for settled promises. A throwing `constructor` getter transfers control to the script exception handler without suspending.
- Snapshot roots include indexed source module environments and namespaces, builtins, class prototypes, host module state slots, pending operations, queued Promise rejection notifications, and caller-supplied script roots. Traverse reachable closure environments, host instance slots, and async continuations. Rejection notifications are delivered at the next successful microtask checkpoint, not by restore. Native pointers, host callbacks, and queued microtasks are not in the snapshot.
- Restore is exact-version: `SNAPSHOT_FORMAT_VERSION`, `BYTECODE_VERSION`, `build_id`, and the binding fingerprint must match. Missing declarations, slot/payload mismatch, or missing operation rejects the whole restore; the VM must not start. No snapshot migration.

Host binding functions are synchronous and must not re-enter the VM. Declarations are immutable and shared; the VM owns prototypes, instances, slots, payloads, module state, and operation records. `JsValue` handles are valid only for the VM generation and call that produced them; retaining a value requires a declared slot or module state slot.

## Key Directories

| Path | Role |
| --- | --- |
| `crates/vm/src` | Bytecode contract, loader checks, interpreter, heap/object table, Promise/microtasks, host protocol, snapshot |
| `crates/compiler/src` | Oxc frontend, lowering, liveness, slot allocation |
| `crates/host-dts/src` | Host `.d.ts` inventory: direct value exports per string module block, located unsupported-syntax diagnostics |
| `crates/binding-macros/src` | Typed authoring macros; `host_bindings!` reads `dts` and checks declared names at expansion |
| `crates/compiler/src/lower` | Only body walker: `stmt`, `expr`, `control`, `pattern` |
| `crates/harness/src` | `TestHost`, directive case runner, demo binary |
| `crates/harness/tests/e2e.rs` | Hand-written snapshot/restore and loader tests |
| `crates/harness/tests/cases` | Semantic `.js` fixtures; one generated `#[test]` each |
- `docs/design.md`|Target design, Chinese. Not a status report|
- `docs/gaps.md`|Current JS-subset implementation gap inventory (what is rejected/missing today, derived from code). Status report; refresh after behavior changes.|
| `docs/implemented.md`|Derived inventory of what the current JS-subset does implement. Status report; refresh after behavior changes.|

`README.md` stays at the overview level: pipeline, how to run, directory layout, and links to the status docs. It MUST NOT contain implementation details (slot/snapshot mechanics, format versions, specific opcodes or built-in lists, per-feature behavior notes) — those belong in `docs/implemented.md` / `docs/gaps.md`, and contract details in this file.

There is no `scripts/` directory and no CI.

## Development Commands

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

Package names are `yield-vm`, `yield-compiler`, `yield-harness` (not the directory names). The demo binary has no arguments.

## Code Conventions & Common Patterns

- Edition 2024. Module docs (`//!`) state invariants; match that voice when adding a module. Code comments are English. README and `docs/design.md` are Chinese.
- Small public surface. Crate internals are `pub(crate)`. New opcodes go in `yield_vm::bytecode::Instr`, then `compiler/src/instr.rs` (uses/defs/throws/rewrite) and the lowerer. CFG and slot allocation must not match on instruction shape.
- Incompatible bytecode changes bump `BYTECODE_VERSION` (`crates/vm/src/bytecode.rs`). Snapshot record-shape changes bump `SNAPSHOT_FORMAT_VERSION` (`crates/vm/src/snapshot/record.rs`). Do not bump them together unless both contracts changed. `Build::finalize` sets `build_id` from `compute_id`; content changes already reject old snapshots.
- Errors: `CompileError` is a located struct (`message`, `line`, `column`, `span`), not a `thiserror` enum. `VmError` is `thiserror` (`InvalidBuild`, `InvalidSnapshot`, `MissingImport` / `MissingOperation`, `Host`, `HostFault`, `UnknownOperation`, `InvalidState`, `Thrown`). Script throws are `Value`s (`ExecOutcome::Thrown`), not `VmError`, until they reject an async result. Host binding functions return `JsResult` (`JsError::Throw` / `JsError::Fault`); host scheduling failures use `HostFault`. `Host::report_error` receives a `JsValue`.
- Ordered maps are `BTreeMap` / `BTreeSet` (imports, globals, operations, natives). Keep that when identity or snapshot order matters.
- `const` mutability is resolved by binding ID at compile time, including through closures. Initialization writes (including loop declarations) are allowed. Assignment to a `const` throws a catchable runtime `TypeError` at the write, after preceding evaluation effects; logical assignment can short-circuit without a write. This does not freeze the bound value. Static TDZ for the current function lives in `compiler/src/env.rs` (`pending_lexicals`), not the emitter. A closure does not inherit that set, so an early cross-closure read is `undefined`. This is not full ECMAScript TDZ, and loop iterations do not get fresh bindings.
- Name policy (`compiler/src/names.rs`): local bindings, static imports (source and host), eligible preload globals, then builtins. Bare preload names read getter-only properties through `globalThis` outside the preload subgraph.
- Host modules use the sorted `CompileOptions.imports: BTreeMap<String, BTreeSet<String>>` manifest. `CompileOptions::from_dts` reads it from the host's `.d.ts`, and `host_bindings! { dts: "path" }` (path relative to the caller's `Cargo.toml`) re-parses the same file during macro expansion, comparing each declared `module:export` against the generated `__YIELD_NAMES` tables. A declared name missing from the implementing `#[host_module]` is `missing host export: module:export` at compile time; extra implementations are allowed and stay out of namespace exports. Harness declarations are `harness/test.d.ts` and `harness/demo.d.ts`; e2e fixtures live in `harness/tests/dts/`, named by their sorted module combination.
- `TestHost` semantics: `seed()` → `10`; `wait(label)` starts the `Wait` operation (display label `wait:{label}`, default recovery `Resume`); `bind(label)` allocates a `Node` instance with payload `label` and method `label()`; `expect(actual)` allocates a `Matcher` instance with slot `actual`, shared methods, and a `value` accessor; `inspect` / `setLabel` → `undefined`. Inject recovery failure with `host.labels().set_reject_restore_operation(true)`; corrupt snapshot payloads by editing the record.

Where to edit:

| Change | Touch |
| --- | --- |
| Syntax subset | `compiler/src/lower` (worklist, not recursive borrow), `frontend.rs`; reject with `unsupported()` |
| Liveness / which slots survive await or yield | `analyze.rs`, `allocate.rs` |
| New opcode | `vm/src/bytecode.rs`, `vm/src/exec.rs` (and `ops.rs` if it is a value op), `compiler/src/instr.rs`, lowerer |
| New heap kind | `vm/src/heap/object.rs` `write_edges`; GC and snapshot capture share the tracer in `vm/src/gc.rs` |
| Snapshot or restore checks | `vm/src/snapshot/` (`record.rs` shapes, `capture.rs` restore); add a rejection test in `harness/tests/e2e.rs` |
| Host protocol | `vm/src/host.rs`, `vm/src/binding/` (`schema.rs` declarations, `call.rs` `CallCx`); implementors: `harness/src/lib.rs`, `harness/src/main.rs` |
| Host declaration syntax | `host-dts/src/lib.rs` (inventory and rejections), `compiler/src/lib.rs` (`from_dts`), `binding-macros/src/registry.rs` (`dts`, const checks), `binding-macros/src/typed.rs` (`__YIELD_NAMES`) |
| Observable JS behavior | a `.js` case, not a new Rust harness |
| ArrayBuffer / DataView / TypedArray member | `vm/src/builtin/binary/` (`buffer.rs`, `view.rs`, `typed.rs`, `element.rs`, `codec.rs`); exotic index lookup lives in `vm/src/heap/binary.rs` |

Easy to break:

- Each source module has separate synchronous declaration and evaluation functions. Hoisted functions close over the module environment before any body runs. Only the entry's linked export table is public to the host. Snapshot module roots and never rerun initialization during restore. Nested functions are a worklist of Oxc nodes plus the captured scope chain.
- Before `allocate`, every `Slot` is a logical register stuffed into `SlotKind::Transient`. `slot.kind` is meaningless until `FrameLayout` remaps it. `promote` rewrites one instruction to one instruction so jump, resume, and handler PCs stay valid.
- `instr.rs::may_throw` must classify any opcode that can deliver a JavaScript exception, or exception edges never reach liveness or promotion. A `VmError` is not a JavaScript exception.
- `Function::needs_environment` is derived from params and code (`GetBinding` and friends). Do not store a forgeable flag. Each module uses one environment if either initializer needs one.
- Build constants and host numbers may be non-finite. Build JSON uses the same special-number markers as snapshot values; finite numbers stay JSON numbers.
- IDs are counters (`ObjectId`, `ContinuationId`) with overflow checks. Snapshot JSON order follows `BTreeMap`.

## Important Files

- `Cargo.toml` — workspace members and pinned deps (`oxc = "=0.159.0"` with `ast_visit`)
- `crates/compiler/src/lib.rs` — pipeline and `compile`
- `crates/host-dts/src/lib.rs` — `parse(source) -> module/export name tables`, located rejections
- `crates/vm/src/lib.rs` — re-exports: `Build`, `Vm`, `Host`, `Snapshot`, `Value`, `VmError`
- `crates/vm/src/bytecode.rs` — `Instr`, `ResumeEntry`, `ExceptionHandler`, `Build::validate`
- `crates/vm/src/vm.rs` — `get_export`, `start`, `run_until_idle`, `complete_operation`
- `crates/vm/src/snapshot/` — `Vm::snapshot`, `Vm::restore`, JSON codec
- `crates/harness/src/case.rs` — `// @` directive runner
- `crates/harness/build.rs` — generates `OUT_DIR/cases.rs`
- `crates/harness/src/main.rs` — the only binary; embedded design-doc example

## Runtime/Tooling Preferences

- Rust only. No npm, Node, or package manager for the engine. JS files are fixtures, compiled by this compiler.
- No `rust-toolchain.toml`, `rustfmt.toml`, `clippy.toml`, `[lints]`, `[features]`, or `.cargo/config.toml`. Use rustfmt and clippy defaults.
- Edition 2024 implies Rust ≥ 1.85. MSRV is not otherwise declared.
- License `MIT OR Apache-2.0`, version `0.1.0`.
- Do not add a dependency unless the bytecode/VM/host boundary needs it. Parser stays Oxc, locked. Do not leak Oxc types past `CompileError`.

## Testing & QA

Four layers, all `cargo test`. No insta, no coverage gate, no `#[ignore]` / `#[should_panic]`, no unit-test modules inside `vm` or `compiler` sources. The VM is tested through `yield-harness`.

1. `crates/compiler/tests/compile.rs` — bytecode shape of the two-await example and unsupported-syntax location.
2. `crates/harness/tests/e2e.rs` — snapshot identity, loader rejection, finally-across-await, reachability. Assert `VmError` with `matches!`.
3. `crates/harness/tests/cases/*.js` — generated tests. `build.rs` requires a non-empty dir of `*.js` whose stems match `[a-z][a-z0-9_]*`. Test function is `case_<stem>`.
4. `crates/host-dts/src/lib.rs` unit tests plus `crates/binding-macros/tests/host_dts_fixture.rs`, which builds a throwaway consumer crate and runs `cargo check` against it to prove declared-but-missing exports fail at macro expansion.

Host declaration fixtures: `crates/harness/test.d.ts` and `crates/harness/demo.d.ts` for the real hosts, `crates/harness/tests/dts/<modules>.d.ts` for e2e hosts (sorted module names joined by `-`; fixtures that link only `TestHost` reuse `harness/test.d.ts` through `support::TEST_DECLARATIONS`), `crates/binding-macros/tests/dts/` for macro tests.

Case contract (`yield_harness::run_case`):

- Leading blank lines and `// @` directives are the header. The whole file, header included, is compiled so diagnostics keep file line numbers.
- Body must explicitly export a callable binding under the public name `run`. The runner always starts `"run"`.
- `@args` JSON array, only as the first directive.
- `@pending ["wait:a"]`, `@restore` (JSON round-trip, then `Vm::restore`), `@fulfill ["wait:a", 2]`, `@reject ["wait:a", "boom"]`.
- `@advance <u64>` advances the test clock by decimal milliseconds and drains work after each due timer.
- Last directive must be `@returns`, `@fulfilled`, `@rejected`, or `@uncaught`. Scalars only: number, bool, string, `null`, `undefined`. `@uncaught` takes an array and checks the exact ordered list of errors reported to the host.
- Fixtures import host exports explicitly from `test`. `gc()` collects synchronously without draining microtasks. `setTimeout` returns a cancellable task handle, `clearTimeout` cancels it, and `sleep` returns a native Promise.

Example:

```js
// @pending ["wait:a"]
// @restore
// @fulfill ["wait:a", 2]
// @fulfilled 2

export async function run() {
  return await wait("a");
}
```

Add a behavior case by dropping `snake_case.js` into a category subdirectory of `crates/harness/tests/cases/` (`arrays`, `async`, `binary`, `closures`, `coercion`, `collections`, `const`, `control`, `descriptors`, `destructuring`, `errors`, `finally`, `functions`, `gc`, `host`, `json`, `loops`, `math`, `numbers`, `objects`, `optional_chain`, `promise`, `prototypes`, `proxy`, `reflect`, `regexp`, `strings`, `switch`, `symbols`). Do not add cases at the top level of `tests/cases/`; keep every fixture in the matching category. `build.rs` walks the directory recursively; the generated test is `case_<category>_<stem>`. `cargo:rerun-if-changed=tests/cases` regenerates tests. Use `e2e.rs` when the assertion needs heap identity, slot tables, or a rejected restore.

## Commit Workflow

Create a git commit immediately after each completed phase of behavior change (new syntax support, VM/snapshot contract changes, host protocol changes, new test cases). Do not accumulate changes into one large commit.

Before committing, verify in order:

1. `cargo fmt --check` and `cargo clippy --workspace --all-targets` pass (run `cargo fmt` first if they report changes).
2. `cargo test --workspace` passes; for contract-related changes also run the matching layer (see Testing & QA).
3. If behavior changed, update docs in sync: refresh `docs/gaps.md` (current-gap inventory) and `docs/implemented.md` (implemented inventory); update `README.md` / `docs/design.md` when the target design is affected, and fix any outdated statements in this file.
4. If bytecode or snapshot record shape changed, check whether `BYTECODE_VERSION` / `SNAPSHOT_FORMAT_VERSION` need a bump (see Code Conventions).

Commit messages are a single imperative sentence in English describing the behavior change itself, e.g. `snapshot: reject restore when natives are missing`. Docs and tests land in the same commit, never split out separately.
