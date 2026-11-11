# Documentation

This repository keeps two complementary sets of documents. They serve different
readers and should not be mixed:

## `skills/` — how to use and embed Zuqe

**Audience: application developers and LLM agents.** These are task-oriented
guides for the public embedding API: setup, execution, native bindings, modules,
Promise and Future bridging, JIT configuration, zapi extensions, and GC safety
rules for host code. Start here when writing Rust code against `zuqe`.
`skills/SKILL.md` is the entry point; the topic pages live in
`skills/references/`.

| Topic | Document |
|---|---|
| Entry point, topic index, boundaries | [zuqe Embedding Skill](../skills/SKILL.md) |
| Runtime/Context, `eval`/`eval_wait`, features, limits | [Basics: Runtime, Context, and Execution](../skills/references/basics.md) |
| Native functions | [Native Function Binding](../skills/references/native-function.md) |
| Native classes | [Native Classes](../skills/references/native-class.md) |
| ESM loading and native modules | [Modules: ESM Loading and Native Modules](../skills/references/native-module.md) |
| Promise and Rust Future | [Promise and Rust Future Bridge](../skills/references/promise.md) |
| Tiered JIT usage | [Tiered JIT Usage](../skills/references/jit.md) |
| zapi `cdylib` extensions | `zuqe-zapi/skills/SKILL.md` — the zapi crate's guide; authoring needs only `zuqe_zapi` |
| GC rules (the only rule source) | [GC Safety Rules](../skills/references/gc-safety.md) |
| GC trace examples | [GC Tracing: Examples and Pitfalls](../skills/references/gc-trace.md) |
| Write barriers, audit, gc-stress | [Write Barriers: Patterns and Tools](../skills/references/gc-write-barrier.md) |
| Workflow and review checklist | [Workflow and Review Checklist](../skills/references/workflow-checklist.md) |

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
| Zapi extensions: host side (loading, registration, lifetime) | [zapi-host.md](zapi-host.md) |
| Build on Windows (MinGW) | [build-windows-gnu.md](build-windows-gnu.md) |
| Cross-compile Windows from Linux/macOS | [build-cross.md](build-cross.md) |
| `zjs` command-line interpreter | [zjs.md](zjs.md) |
| Known release fat-LTO miscompile | [known-issue.md](known-issue.md) |

These older standalone API pages now contain a short pointer only. The full
content is in `skills/`:

- [basic.md](basic.md) -> [Basics: Runtime, Context, and Execution](../skills/references/basics.md)
- [native-function.md](native-function.md) -> [Native Function Binding](../skills/references/native-function.md)
- [native-classe.md](native-classe.md) -> [Native Classes](../skills/references/native-class.md)
- [native-module.md](native-module.md) -> [Modules: ESM Loading and Native Modules](../skills/references/native-module.md)
