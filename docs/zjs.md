# ZJS - Zuqe JavaScript Shell

ZJS is a command-line JavaScript interpreter built on top of the Zuqe engine. It provides a comprehensive runtime environment for executing JavaScript code with access to native system functionality.

## Usage

Suppose you have got `zjs` binary, use it like:

```bash
zjs [options] <filename>
```

### Options

- **`-m, --module`**: Load the script as an ES6 module. When this flag is set, the script will be parsed and executed using ES6 module semantics instead of global script execution.

- **`--memory-limit <n>`**: Set the memory (GC heap) limit to `n` bytes. Size suffixes `k`/`K`, `m`/`M`, `g`/`G` are supported (multiply by 2^10 / 2^20 / 2^30).

- **`--vm-stack-arena <n>`** _(default: 256k)_: Set the size of the JS virtual machine **stack arena** to `n` bytes. This is a pre-allocated, contiguous bump-region used as the _fast path_ for function call frames — frames are carved out of it by simply bumping a pointer.

- **`--vm-stack-limit <n>`** _(default: 512k)_: Set the **hard cap on the total live virtual stack** (arena-used + heap-used) to `n` bytes. When a new frame would push the combined usage past this limit, a stack-overflow error is thrown. This is what actually bounds recursion depth.

- **`--thread-stack-limit <n>`**: Set the **C-stack recursion limit** — the maximum native stack bytes JS recursion may consume below the eval-entry SP. Omitted or `0` selects **auto mode**: the guard is derived from the current thread's real stack bottom, keeping a fixed internal head-room (`THREAD_STACK_LIMIT_MARGIN`) free for the throw path. Auto mode is the default; a strict limit only applies when set explicitly.

  Examples:

  ```bash
  zjs --vm-stack-arena 1m script.js       # 1 MiB VM stack arena
  zjs --vm-stack-limit 512k script.js     # 512 KiB total virtual-stack cap
  zjs --thread-stack-limit 2m script.js    # 2 MiB C-stack recursion guard
  ```

#### How `--vm-stack-arena` and `--vm-stack-limit` interact

Frames are always allocated from the arena first (fast, no heap allocation). Once the arena is full, additional frames **spill over to individual heap allocations**, and this continues until the total live virtual stack (arena-used + heap-used) reaches `--vm-stack-limit`, at which point a stack-overflow error is raised.

Because the arena can never hold more than its own size, and any usage beyond the arena comes from heap spill-over, **`--vm-stack-limit` should always be `>= --vm-stack-arena`**:

| Relationship     | Effect                                                                                                                                                                        |
| ---------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `limit < arena`  | The limit is hit while still inside the arena, so the heap spill-over path is never reached and the arena memory above `limit` is simply **wasted** (allocated but unusable). |
| `limit == arena` | All frames live in the fast arena; the stack **never spills to the heap** and the arena is fully utilized. A clean, allocation-free configuration.                            |
| `limit > arena`  | The arena acts as a fast cache and the heap provides extra headroom up to `limit`. **This is the intended design** — hence the defaults `arena=256k`, `limit=512k`.           |

In short: set `--vm-stack-arena` to the size that comfortably covers typical call depth (for speed), and set `--vm-stack-limit` to the maximum depth you are willing to tolerate before aborting (for safety). Setting `limit` below `arena` only wastes memory.

### Script Execution

ZJS can execute JavaScript files in two modes:

- **Global Mode** (default): The script is executed in the global eval mode
- **ES6 Module Mode** (with `-m` or `--module` flag): The script is parse and loaded as an ES6 module with import/export support

In case no `-m` or `--module` flag, the execution mode will be automatically detected:

- for filename with `.mjs` extension, or `import/export` syntax is found in file content, then use ES6 module mode
- otherwise use global mode.

### Example Usage

```bash
# Execute a regular JavaScript file
zjs hello.js

# Execute as ES6 module
zjs -m app.js

# Set custom VM stack arena
zjs --vm-stack-arena 2m my-script.js

# Execute with custom stack arena and module mode
zjs --vm-stack-arena 1m -m esm-script.js
```

### Interactive REPL

ZJS include a script file `scripts/repl.js` that provides an interactive JavaScript environment, a.k.a REPL (Read-Eval-Print Loop).

Launch it using:

```bash
zjs -m scripts/repl.js
```

#### REPL Features

- **Ctrl-D or Ctrl-C**: To exit
- **Tab Completion**: Press `Tab` to auto-complete variable names, function names, and object properties
- **Syntax Highlighting**: The REPL provides colorized output with syntax highlighting for better readability
- **Command History**: Navigate through previous commands using arrow keys (↑/↓)
- **Multi-line Editing**: Support for entering multi-line JavaScript expressions
- **Error Handling**: Comprehensive error reporting with stack traces

#### REPL Commands

The REPL supports several special commands (prefixed with `\`):

- **`\h`** or **`\?`**: Display help information
- **`\x`**: Toggle hexadecimal number display
- **`\d`**: Toggle decimal number display (default)
- **`\t`**: Toggle timing display
- **`\clear`**: Clear the terminal
- **`\q`**: Exit the REPL

#### Key Bindings

- **`Ctrl+C`**: Interrupt current execution (press twice to quit)
- **`Ctrl+D`**: Delete character or exit (when line is empty)
- **`Ctrl+A`**: Move to beginning of line
- **`Ctrl+E`**: Move to end of line
- **`Ctrl+B`**: Move backward one character
- **`Ctrl+F`**: Move forward one character
- **`Ctrl+K`**: Delete to end of line

The REPL implementation in `scripts/repl.js` is based on and modified from QuickJS's REPL, providing a familiar and feature-rich interactive JavaScript experience.

## ZJS build from source

ZJS is located at `examples/zjs`, to build it, using following command:

```bash
cargo build --release --examples
```

This will result `target/release/examples/zjs`, which is the CLI executable.

## Source code Explain

The ZJS example demonstrates how to embed the Zuqe engine in a Rust application. Here's an overview of the key components:

1. **Main Entry Point** (`main.rs`):
   - Command-line argument parsing using `clap`
   - Script file loading and execution
   - Runtime configuration (memory limit, stack arena/limit/overflow, module loading)
   - Error handling and result display
   - Auto-detection of module mode (`.mjs` extension, `-m` flag, or `import`/`export` syntax in source)

2. **Global Functions** (`global.rs`):
   - Implementation of built-in globals: `print()`, the `console` object (`log`/`debug`/`info`/`warn`/`error`), and `scriptArgs`
   - Optional `dumpValue`/`restoreValue` for bytecode persistence (behind the `persistent` feature)

3. **Native class `File`** (`class_file.rs`):
   - Custom `File` class implementation with `FileDesc` enum (Stdin/Stdout/Stderr/File)
   - File I/O operations: `fileno`, `readAsString`, `puts`, `flush`, `close`

4. **Native class `Emitter`** (`class_emitter.rs`):
   - Simple event emitter class with `on` and `emit` methods
   - Constructor exposed to global scope via `ClassCtorDef::expose_to_global()`

5. **Value Inspector** (`inspect.rs`):
   - Node.js `util.inspect`-compatible formatter
   - Handles all JS types including circular references, Map/Set, TypedArray, RegExp, etc.
   - Configurable depth, colors, and formatting options

6. **Native module `os`** (`mod_os.rs`):
   - OS-specific functionality: `now`, `isatty`, `ttyGetWinSize`, `ttySetRaw`
   - I/O handlers: `setReadHandler`, `setWriteHandler`, `read`, `write`
   - Signal handling: `signal`, plus signal constants (`SIGINT`, `SIGTERM`, etc.)
   - Event loop polling via `select` (Unix) / `WaitForMultipleObjects` (Windows)
   - GC trace for thread-local handlers and timers

7. **Native module `std`** (`mod_std.rs`):
   - Standard library: `open`, `puts` (stdout), `evalScript`, `gc`, `exit`
   - Lazy `in`/`out`/`err` values for pre-built `File` instances

The main application flow:

1. **Initialize Runtime**: Create a new Zuqe runtime instance and install the panic handler
2. **Configure Limits**: Parse and set memory limit, stack arena size, stack limit, and C-stack overflow limit
3. **Setup Module Loader**: Configure custom module loading mechanism (including zapi extension loading)
4. **Configure GC Trace**: Register custom GC trace handler for thread-local JS handles
5. **Resolve Native Class Ids**: Host classes (`File`, `Emitter`) declared with `def_native_object!` are collected into `HOST_CLASS_DEFS` at link time and assigned ids during engine init; fetch them via `JSFile::class_id()` / `JSEmitter::class_id()`
6. **Create Context**: Establish execution context with ECMA standard setup
7. **Enable Classes**: Enable native classes in the context via `cx.use_class()`
8. **Initialize Modules**: Load OS and standard library modules
9. **Setup Globals**: Configure global objects and functions (including `scriptArgs`)
10. **Execute Script**: Run the JavaScript code in the appropriate mode (global/module)

```rust
// Create runtime
let mut rt = Runtime::create().unwrap();

// Install panic handler for release builds
rt.install_panic_handler();

// Optional: memory / stack limits (parsed from `--memory-limit`,
// `--vm-stack-arena`, `--vm-stack-limit`, `--thread-stack-limit`)
// rt.gc_set_memory_limit(limit_in_bytes);
// rt.set_vm_stack_arena(1 << 20);       // 1 MiB VM stack arena
// rt.set_vm_stack_limit(512 << 10);     // 512 KiB value-stack limit
// rt.set_thread_stack_limit(512 << 10);  // omit/0 = auto mode (from thread stack bottom)

// Setup module loader (supports .js, .mjs, and zapi extensions .dylib/.so/.dll)
rt.set_module_loader(js_module_loader);

// Customized GC trace handler (see docs/gc.md)
rt.gc_set_trace_handler(|tr| {
    mod_os_gc_trace(tr);
});

// Auto-detect module mode: explicit `-m`, `.mjs` extension, or
// `import`/`export` syntax in the source.
let is_module = args.module
    || args.filename.ends_with(".mjs")
    || ModuleDef::detect_module(script.as_bytes());

// Native classes declared with `def_native_object!` are collected into the
// process-global `HOST_CLASS_DEFS` slice and get their ids during engine
// init; just read them back here.
let class_id_file = JSFile::class_id();
let class_id_emitter = JSEmitter::class_id();

// Create execution context and set up environment
let ctx = rt
    .create_context(|cx| {
        // ECMA standard stuff
        ScopedContext::ECMA_CONTEXT_SETUP(cx)?;

        // enable the native classes in this context
        cx.use_class(class_id_file)?;
        cx.use_class(class_id_emitter)?;
        init_mod_std(cx)?;
        init_mod_os(cx)?;
        setup_globals(cx, &args.script_args)
    })
    .unwrap();

// Execute the script — this final step runs inside the async `main` (see
// "Async mode" below). The entry name must be a canonical `file://` URL so
// that relative dynamic `import()` and `import.meta.url` resolve correctly.
let entry_name =
    zuqe::path_to_url(&args.filename).expect("entry path must convert to a file:// URL");
let run_result = ctx
    .eval_wait(
        &mut rt,
        script.as_bytes(),
        &EvalName::BaseUrl(entry_name),
        if is_module { EvalType::Module } else { EvalType::Global },
        EvalFlags::empty(),
    )
    .await;
```

The ZJS example serves as both a practical command-line tool and an educational resource for understanding how to integrate JavaScript capabilities into Rust applications using the Zuqe engine.

### Async mode with `async_support`

When zjs is compiled with the `async_support` Cargo feature enabled (default in
the workspace), the main function switches to an async entry point using Tokio.
It uses the **async** `Context::eval_wait()`, which evaluates the entry and
drives the event loop to completion:

```rust
#[cfg(feature = "async_support")]
#[tokio::main]
async fn main() -> ExitCode {
    let (mut rt, mut ctx, args, script, is_module) = run_zjs_prepare();

    // Convert the entry filename to a canonical `file://` URL. `module_name`
    // (and `import.meta.url`) is a URL, and a `file://` base is also required
    // so that dynamic `import()` from a *script* entry resolves relative
    // specifiers correctly instead of being handed a bare filesystem path.
    let entry_name =
        zuqe::path_to_url(&args.filename).expect("entry path must convert to a file:// URL");

    // Eval the entry and drive the event loop to completion. `eval_wait`
    // handles top-level module Promise observation internally via
    // `futures_util::future::select` (never `tokio::join!`), so a rejected
    // top-level promise is surfaced as `ExitStatus::Exception` and never
    // silently dropped. A synchronous (non-Promise) result is returned as
    // `ExitStatus::Value` for the host to print.
    let run_result = ctx
        .eval_wait(
            &mut rt,
            script.as_bytes(),
            &EvalName::BaseUrl(entry_name),
            if is_module { EvalType::Module } else { EvalType::Global },
            EvalFlags::empty(),
        )
        .await;

    match run_result {
        ExitStatus::Value(v) => {
            // A module (or a script with no meaningful tail value) evaluates to
            // `undefined`; printing it is just noise. Only surface real values,
            // matching how a REPL/CLI treats an absent result.
            if !v.is_undefined() {
                ctx.with_root_scope(|mut cx| {
                    colorize_println(
                        inspect::js_inspect_value(
                            &mut cx,
                            v,
                            &inspect::InspectOptions::default(),
                        ),
                        GreenFg,
                    );
                });
            }
            ExitCode::SUCCESS
        }
        ExitStatus::Interrupted(code) => {
            #[cfg(debug_assertions)]
            log::debug!("Process interrupted with code {:?}", code);
            code
        }
        ExitStatus::Exception(Exception::OutOfMemory) => {
            colorize_println("out of memory", RedFg);
            ExitCode::FAILURE
        }
        ExitStatus::Exception(Exception::TooManyAtoms) => {
            colorize_println("too many atoms", RedFg);
            ExitCode::FAILURE
        }
        ExitStatus::Exception(Exception::Error(err)) => {
            ctx.with_root_scope(|mut cx| {
                colorize_println(err.to_string(&mut cx).unwrap().encode_utf8(false), RedFg);
            });
            ExitCode::FAILURE
        }
        ExitStatus::Exception(Exception::No | Exception::TooManyClasses) => {
            unreachable!();
        }
    }
}
```

Key differences from the synchronous variant:

- `#[tokio::main]` makes the entry point async.
- The async `ctx.eval_wait(&mut rt, ...)` is used instead of the blocking
  `eval_wait()` — it drives the runtime's tick loop as a `Future` and observes
  the top-level Promise, so a rejected module entry surfaces as
  `ExitStatus::Exception` instead of being silently dropped.
- `Promise::into_future()` is still available to convert individual JS
  Promises into Rust `Future`s that can be `await`ed directly (see
  [Basic Usage](basic.md) and [Promise](promise.md)).
