# Zapi Extensions: Host Side

What the engine does with a zapi extension: finding it, loading it, registering
it, and keeping it alive. Writing one is the other side of the boundary and lives
with the crate that does it — see below.

A zapi extension is a dynamic library (`.so` / `.dylib` / `.dll`) that the host
loads and exposes as a JavaScript module:

```js
import * as my from "./libmymod.so";
my.add(2, 3); // 5
```

This page is the **host** side only: how zuqe finds, registers and keeps such a
library. Authoring an extension needs none of it — it needs only the `zuqe_zapi` +
`zuqe_zapi_macros` crates — so the authoring guide lives with that crate:
**`zuqe-zapi/skills/SKILL.md`** (a sibling repository).

## What the host does

1. Build with `--features zapi`.
2. Install a module loader that recognizes the `.so` / `.dylib` / `.dll` suffix
   and calls `ScopedContext::load_zapi_extension(path)`. Without a loader every
   import of a library throws. `zjs` (`examples/zjs`) does this.
3. `load_zapi_extension` canonicalizes the path, dlopens it once per process,
   calls `zq_module_register`, checks the reported ABI/API versions against this
   build's accepted window (`ZQ_ABI_MIN..=ZQ_ABI_VERSION`,
   `ZQ_API_MIN..=ZQ_API_VERSION`), then registers the exported classes and
   publishes each exported field in the module namespace.

Everything after that is Promise/tick/GC work covered by the other pages here:
the library stays mapped for the process lifetime (the host holds pointers into
its static data), zapi classes take part in GC through their `gc_trace` callback,
and — with `gc_generations` — live extension instances are re-scanned on a minor
collection because the zapi ABI has no write-barrier entry point.

Implementation: `src/zapi/host.rs` (loading, symbol resolution, class and export
registration) and `src/zapi/stub.rs` (the `ZqApi` function table).

## Runnable references

| Example | What it shows |
|---|---|
| `examples/zapi_basic` | Every JS value type crossing the boundary, a GC-traced class, a value export |
| `examples/zapi_event` | A full class: methods, `magic` dispatch, static accessors, class-level flag |
| `examples/zapi_ffi_test` | Every `ZqApi` entry point exercised; driven by `test_ffi.mjs` (conformance) |
| `examples/zapi_raw` | The raw ABI, if you need to drop below the safe layer |
