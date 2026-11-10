# Documentation

This repository keeps two complementary sets of documents. They serve different
readers and should not be mixed:

## `skills/` — how to use and embed Zuqe

**Audience: application developers and LLM agents.** These are task-oriented
guides for the public embedding API: setup, execution, native bindings, modules,
Promise/Future bridging, JIT configuration, zapi extensions, and GC rules for
host code. Start here when writing Rust code against `zuqe`.

| Topic | Document |
|---|---|
| Entry point, topic index, boundaries | [skills/SKILL.md](../skills/SKILL.md) |
| Runtime/Context, `eval`/`eval_wait`, features, limits | [skills/embedding-basics.md](../skills/embedding-basics.md) |
| Native functions | [skills/native-function.md](../skills/native-function.md) |
| Native classes | [skills/native-class.md](../skills/native-class.md) |
| ESM loading and native modules | [skills/native-module.md](../skills/native-module.md) |
| Promise ↔ Rust Future | [skills/promise.md](../skills/promise.md) |
| Tiered JIT usage | [skills/jit.md](../skills/jit.md) |
| zapi `cdylib` extensions | [skills/zapi-extension.md](../skills/zapi-extension.md) |
| GC rules (the only rule source) | [skills/gc-safety.md](../skills/gc-safety.md) |
| GC trace examples | [skills/gc-trace.md](../skills/gc-trace.md) |
| Write barriers / audit / gc-stress | [skills/gc-write-barrier.md](../skills/gc-write-barrier.md) |
| Workflow and review checklist | [skills/workflow-checklist.md](../skills/workflow-checklist.md) |

## `docs/` — internals, design, and reference

**Audience: engine contributors, maintainers, and hosts that need the why/how.**
These documents describe how Zuqe is built — object model, GC implementation,
tick loop internals, module graph, JIT pipeline, known toolchain issues — plus
build instructions and the bundled `zjs` CLI manual. User-facing API recipes live
in `skills/`; the pages below link there instead of duplicating them.

| Topic | Document |
|---|---|
| About Zuqe: name, goals, design philosophy | [about.md](about.md) |
| Garbage collection internals | [gc.md](gc.md) |
| Running-loop / tick internals | [loop.md](loop.md) |
| Tiered JIT design and pipeline | [jit.md](jit.md) |
| Module system architecture | [module.md](module.md) |
| Promise internals | [promise.md](promise.md) |
| Build on Windows (MinGW) | [build-windows-gnu.md](build-windows-gnu.md) |
| Cross-compile Windows from Linux/macOS | [build-cross.md](build-cross.md) |
| `zjs` command-line interpreter | [zjs.md](zjs.md) |
| Known release fat-LTO miscompile | [known-issue.md](known-issue.md) |
