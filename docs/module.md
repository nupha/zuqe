# Module System (internals)

This page describes how Zuqe represents, resolves, links, and evaluates ES
modules. For the host-facing API — `set_module_loader`,
`set_module_normalizer`, `create_module`, import attributes, `import.meta` — see
**[Modules: ESM loading and native modules](../skills/native-module.md)**.

## Representation

A module is a GC node wrapper around `ModuleDefImpl` (`src/module/mdef.rs`):

- `module_name: Atom` — the identity/cache key (a canonical URL for ESM).
- `dependencies: Vec<ModuleDependency>` — static imports.
- `import_entries` / `export_entries` / `star_export_entries` — the module
  record's import/export tables.
- `module_ns: WriteBarrier<Option<Object>>` — lazily built namespace object.
- `bytecode: WriteBarrier<JSValue>` — for JS modules, either a `Bytecode` or a
  `BCWrapper` once the function object has been created.
- `init_func: Option<Box<dyn ModuleInitFn>>` — native modules only; runs at
  evaluation time to install exports.
- `status: JSModuleStatus` — `UNLINKED` → `LINKING` → `LINKED` →
  `EVALUATING` / `EVALUATING_ASYNC` → `EVALUATED`.
- DFS fields (`dfs_index`, `dfs_ancestor_index`, `stack_prev`, `cycle_root`) and
  `async_parent_modules` support cycle handling and top-level await.
- `has_tla` — whether the module body contains `await`.

`ModuleDef` itself is `GcRef<ModuleDefImpl>`; `From<ModuleDef> for JSValue`
creates a `Module`-tagged JS value. All module records are rooted through the
context's `loaded_modules` table.

## Resolution

`js_host_resolve_imported_module` (`src/module/reqres.rs`) handles a static
import:

1. **Native modules first.** The specifier is interned verbatim and looked up in
   `loaded_modules`. If it is a native module (`ModuleDef::is_native()`), it is
   returned *before* normalization, so a bare name such as `"os"` is not turned
   into a filesystem path.
2. **Normalize.** The runtime's `module_normalize_func` (a plain `fn` pointer)
   maps `(base_url, specifier)` to a canonical `Url`. The default joins relative
   specifiers against the importer URL and converts absolute paths to URLs.
   `file://` URLs are canonicalized centrally after normalization, so symlink
   aliases collapse to one cache key.
3. **Look up / load.** If the normalized URL is already in `loaded_modules`, it
   is reused. Otherwise `module_load_func` is called with the URL and the import
   attributes `JSValue`. The default loader is a stub that throws
   `ReferenceError` for every import.
4. **Compile.** A JS loader normally calls
   `ScopedContext::compile_module(source, module_url, strip)`, which produces a
   `ModuleDef` with `EvalType::Module` and `ExecMode::STRICT`.

Dynamic `import()` follows the same resolve/load path at runtime.

## Linking

`ModuleDef::resolve_deps` loads direct dependencies, then `link`
(`src/module/resolve.rs`) performs the recursive linking pass:

- Each module gets a DFS index; `stack_prev` and `cycle_root` identify strongly
  connected components.
- Import/export entries are resolved to slot accesses; `export *` is collected
  in `star_export_entries`.
- A module becomes `LINKED` only after its dependencies are linked.
- Link errors are cached on the module (`set_eval_error`) so subsequent imports
  throw the same error instead of retrying.

## Evaluation

`ModuleDef::evaluate` (`src/module/eval.rs`) returns a Promise:

- A JS module body runs as a function (or async function when `has_tla`) in the
  module's own environment; the completion value becomes the module's
  evaluation result.
- Native modules run `init_func`, which installs the `FieldDef` export values —
  no bytecode is involved.
- Cyclic imports use the SCC root recorded during linking; a module in
  `EVALUATING_ASYNC` is represented by its cycle root.
- Top-level `await` transitions the module to `EVALUATING_ASYNC`; dependent
  modules are tracked in `async_parent_modules` and evaluated when it settles.
- A rejected top-level evaluation promise is surfaced by the host entry
  (`eval_wait`) as `ExitStatus::Exception`, not silently dropped.

## `import.meta`

Each module owns an `import.meta` object. `set_import_meta` installs:

- `url` — the module's `module_name` (so `import.meta.url === module_name`).
- `main` — whether this is the main entry module.

Static imports wire `import.meta` for every module, not only the entry, so
`import.meta.url` also holds inside dependency modules. The object is reached at
runtime through the current module frame / async callback context.

## Native modules

A native module is a `ModuleDef` with `init_func = Some(...)` and no bytecode.
`ScopedContext::create_module(name, entries)` creates it and registers it in
`loaded_modules` under the verbatim name. At evaluation, `set_export_values`
walks the `FieldDef` table and installs functions, constants, lazy values, and
class constructors.

`loaded_module_names(ModuleNameFilter)` exposes the registry for hosts; the
filter distinguishes native (`NativeOnly`), bytecode (`BytecodeOnly`), and all
modules.

## Cache and lifetime

- `loaded_modules` is the single module cache, keyed by the normalized module
  name (or the verbatim native name).
- `js_free_modules` removes modules according to the teardown flag (`ALL` or
  `NOT_RESOLVED`); module records are GC nodes, so their lifetimes also follow
  the GC graph.
- zapi extension libraries are cached process-wide by the host loader, not in
  `loaded_modules`; the module wrapper is a normal native module.
