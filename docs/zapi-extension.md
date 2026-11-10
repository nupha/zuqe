# Writing a Zapi Extension

A zapi extension is a dynamic library (`.so` / `.dylib` / `.dll`) that the host
loads and exposes as a JavaScript module:

```js
import * as my from "./libmymod.so";
my.add(2, 3); // 5
```

Loading is the **host's** job: it enables the `zapi` feature and installs a module
loader that recognizes the extension suffix and calls
`ScopedContext::load_zapi_extension`. `zjs` (`examples/zjs`) does both.

The complete authoring guide — `cdylib` setup, `#[zq_function]` /
`#[zq_export]` / `#[zq_class]` / `#[zq_method]` / `#[zq_static_method]`, the
module manifest and `finalize` hook, `ZqGcTrace` / `payload_no_gc`, async work
(`promise_threaded`, `promise_fut`, async iterators, jobs), and the host loading
contract — is in:

- **[Writing a zapi extension](../skills/zapi-extension.md)**

Runnable references:

| Example | What it shows |
|---|---|
| `examples/zapi_basic` | Every JS value type crossing the boundary, a GC-traced class, a value export |
| `examples/zapi_event` | A full class: methods, `magic` dispatch, static accessors, thread-local state |
| `examples/zapi_raw` | The raw ABI, if you need to drop below the safe layer |

The runtime side of loading (`load_zapi_extension`, symbol resolution, class and
export registration, payload re-scan) is implemented in `src/zapi/host.rs`.
