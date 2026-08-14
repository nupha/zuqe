# About Zuqe

> A lightweight JavaScript engine for Rust — a reimagining of the embeddable engine, and memory‑safe by design.

![Zuque logo](zuqe-logo.png)

## The Name: 朱雀 (Zhuque)

**Zuqe** (pronounced `/zuːk/` — like "zook") is named after **朱雀 (Zhuque)**, the Vermilion Bird of Chinese mythology. Its three defining symbols — **fire, rebirth, and guardianship** — map cleanly onto what Zuqe is:

### Fire → fast

The Vermilion Bird is a creature of fire: brilliant, immediate, alive. Zuqe inherits that temperament. It is a _small, energetic runtime_ — an optimized bytecode interpreter accelerated by **hidden classes & inline caches**, a compact **NaN‑boxed** value representation, and **string ropes** that avoid eager copying. It does more with less, like a flame that needs no heavy fuel.

### Rebirth → a reimagining of the embeddable JS engine

The bird dies in fire and is reborn — a fresh start, free of the old constraints. In the spirit of **QuickJS**, Zuqe is a _Rust‑native reimagining_ of the embeddable JavaScript engine: the same philosophy (small, fast, easy to embed), reborn with Rust's memory safety, a **stackless interpreter** that removes host‑stack overflow risk entirely, and a **bidirectional Promise↔Future bridge** for first‑class async interop. A familiar idea, rebuilt lighter.

### Guardianship → memory safe

As a divine guardian, the Vermilion Bird protects. Zuqe protects the host: it is written in **Rust**, so memory safety is guaranteed by construction, and its **stackless interpreter** keeps call frames off the native stack, meaning deep recursion can _never_ overflow or crash the host process. Safety is not an add‑on; it is the creature's nature.

## What is Zuqe?

Zuqe is a **lightweight JavaScript engine implemented in Rust**. It is designed to be embedded into Rust applications with low memory overhead and a small binary size, balancing **efficiency, safety, and ease of embedding**.

- Implements a comprehensive set of **ES2024** features.
- Passes a **substantial portion** of the [Test262](https://github.com/tc39/test262) conformance suite.
- Ships a rich embedding API: native functions, native classes, native ES modules, scoped execution, and configurable garbage collection.

## Design Philosophy

Zuqe is built around a small set of convictions:

- **Embeddable first.** The primary user is a Rust developer who wants to script their application — not someone building a standalone browser. Every API decision optimizes for that embedding relationship.
- **Safe by construction.** Rust's ownership model plus a stackless interpreter means the host application stays protected even when untrusted or buggy scripts run.
- **Small and trimmable.** You should only pay for what you use. Built‑in objects and full Unicode support are individually gated behind Cargo features, so embedded targets can strip the engine down to the bone.
- **Fast where it counts.** Predictable property access (hidden classes + inline caches), cache‑friendly values (NaN‑boxing), and lazy strings (ropes) keep real‑world workloads smooth without a JIT.

## Architecture Highlights

Engine‑level design choices that set Zuqe apart:

- **Stackless Interpreter** — Call frames live in a managed bump‑arena (spilling to the heap), so deep recursion can't overflow the host stack and stack usage is bounded by a precise, configurable quota.
- **Hidden Classes & Inline Caches** — Objects share `Shape`s and use per‑opcode inline caches for fast, predictable property access.
- **String Ropes** — Concatenations are kept as lazy ropes rather than eagerly copied, keeping string operations memory‑efficient.
- **NaN‑Boxing Values** — JS values are encoded in a compact 64‑bit NaN‑boxed representation for cache‑friendly, low‑overhead storage.
- **Scope‑Based Garbage Collection** — Generational, scope‑bounded GC with a dedicated StackFrame GC partition.
- **Bidirectional Async Bridge** — JS `Promise`s and Rust `Future`s interoperate in both directions (feature `async_support`).

## When to reach for Zuqe

- **Scripting for Rust Services** — Hot‑reload business logic, plugins, and user scripts into your Rust application without restarting it.
- **Edge & Embedded Runtimes** — A tiny binary and low memory footprint let Zuqe fit IoT and edge devices.
- **Developer Tools & Sandboxes** — Safe in‑process scripting for IDEs, CLIs, and build tooling.

---

_Zuqe: symbol of fire, rebirth, and guardianship._
