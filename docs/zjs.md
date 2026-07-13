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

- **`--stack-arena <n>`** *(default: 256k)*: Set the size of the JS virtual machine **stack arena** to `n` bytes. This is a pre-allocated, contiguous bump-region used as the *fast path* for function call frames — frames are carved out of it by simply bumping a pointer.

- **`--stack-limit <n>`** *(default: 512k)*: Set the **hard cap on the total live virtual stack** (arena-used + heap-used) to `n` bytes. When a new frame would push the combined usage past this limit, a stack-overflow error is thrown. This is what actually bounds recursion depth.

- **`--stack-overflow <n>`** *(default: 512k)*: Set the **C-stack recursion limit** (the maximum native stack depth before a stack-overflow guard trips) to `n` bytes. This guards the host's real machine stack, independent of the virtual stack above.

  Examples:

  ```bash
  zjs --stack-arena 1m script.js       # 1 MiB VM stack arena
  zjs --stack-limit 512k script.js     # 512 KiB total virtual-stack cap
  zjs --stack-overflow 2m script.js    # 2 MiB C-stack recursion guard
  ```

#### How `--stack-arena` and `--stack-limit` interact

Frames are always allocated from the arena first (fast, no heap allocation). Once the arena is full, additional frames **spill over to individual heap allocations**, and this continues until the total live virtual stack (arena-used + heap-used) reaches `--stack-limit`, at which point a stack-overflow error is raised.

Because the arena can never hold more than its own size, and any usage beyond the arena comes from heap spill-over, **`--stack-limit` should always be `>= --stack-arena`**:

| Relationship | Effect |
| --- | --- |
| `limit < arena` | The limit is hit while still inside the arena, so the heap spill-over path is never reached and the arena memory above `limit` is simply **wasted** (allocated but unusable). |
| `limit == arena` | All frames live in the fast arena; the stack **never spills to the heap** and the arena is fully utilized. A clean, allocation-free configuration. |
| `limit > arena` | The arena acts as a fast cache and the heap provides extra headroom up to `limit`. **This is the intended design** — hence the defaults `arena=256k`, `limit=512k`. |

In short: set `--stack-arena` to the size that comfortably covers typical call depth (for speed), and set `--stack-limit` to the maximum depth you are willing to tolerate before aborting (for safety). Setting `limit` below `arena` only wastes memory.

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
zjs --stack-arena 2m my-script.js

# Execute with custom stack arena and module mode
zjs --stack-arena 1m -m esm-script.js
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
- **`Ctrl+U`**: Delete to beginning of line

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
   - Runtime configuration (stack size, module loading)
   - Error handling and result display

2. **Global Functions** (`global.rs`):
   - Implementation of built-in globals: `print()`, the `console` object (`log`/`debug`/`info`/`warn`/`error`), and `scriptArgs`
   - Integration with the Zhuque runtime

3. **Native class `File`** (`class_file.rs`):
   - Custom `File` class implementation
   - File I/O operations integration
   - Bridge between JavaScript and Rust file operations

4. **Native module `os`** (`mod_os.rs`):
   - OS-specific functionality exposure to JavaScript
   - Time operations (`os.now()`)
   - Terminal operations (`os.isatty()`, `os.ttyGetWinSize()`)
   - Signal handling for interrupt management

5. **Native module `std`** (`mod_std.rs`):
   - Standard library functions exposed to JavaScript
   - Additional utility functions for script execution

The main application flow:

1. **Initialize Runtime**: Create a new Zhuque runtime instance
2. **Configure Stack Size**: Parse and set the virtual machine stack size
3. **Setup Module Loader**: Configure custom module loading mechanism
4. **Register Native Classes**: Register custom JavaScript classes (e.g., `File`)
5. **Create Context**: Establish execution context with proper configuration
6. **Initialize Modules**: Load OS and standard library modules
7. **Setup Globals**: Configure global objects and functions
8. **Execute Script**: Run the JavaScript code in the appropriate mode (global/module)

```rust
// Create runtime
let mut rt = Runtime::create()?;

// Configure memory / stack limits (all optional)
// rt.gc_set_memory_limit(limit_in_bytes);
// rt.set_stack_arena(1 << 20);       // 1 MiB VM stack arena
// rt.set_stack_limit(512 << 10);     // 512 KiB value-stack limit
// rt.set_stack_overflow(512 << 10);  // 512 KiB C-stack recursion guard

// Setup module loader
rt.set_module_loader(None, js_module_loader, std::ptr::null_mut());

// Register native classes
let class_id_file = JSFile::register(&mut rt)?;
let class_id_emitter = JSEmitter::register(&mut rt)?;

// Create execution context and set up environment
let mut ctx = rt.create_context(|cx| {
    // ECMA standard stuff
    ScopedContext::ECMA_CONTEXT_SETUP(cx)?;

    // enable the native classes in this context
    cx.use_class(class_id_file)?;
    cx.use_class(class_id_emitter)?;
    init_mod_std(cx)?;
    init_mod_os(cx)?;
    setup_globals(cx, &args.script_args)
})?;

// Execute script inside a scope
ctx.with_new_scope(|mut scope| {
    scope.eval_wait(script_bytes, filename, eval_type, eval_flags)
})
```

The ZJS example serves as both a practical command-line tool and an educational resource for understanding how to integrate JavaScript capabilities into Rust applications using the Zuqe engine.

### Async mode with `async_support`

When zjs is compiled with the `async_support` Cargo feature enabled (default in
the workspace), the main function switches to an async entry point using Tokio.
Instead of `eval_wait()`, it uses the non-blocking `eval()` and converts the
returned JS Promise into a Rust `Future` via `Promise::into_future()`:

```rust
#[cfg(feature = "async_support")]
#[tokio::main]
async fn main() -> ExitCode {
    let (mut rt, mut ctx, args, script, is_module) = run_zjs_prepare();

    // The entry filename must be a `file://` URL so that relative dynamic
    // `import()` and `import.meta.url` resolve correctly.
    let entry_name = zuqe::path_to_url(&args.filename)
        .expect("entry path must convert to a file:// URL")
        .to_string();

    // Non-blocking eval: returns immediately with a JS value (possibly a Promise).
    let result = ctx.eval(
        script.as_bytes(),
        &entry_name,
        if is_module { EvalType::Module } else { EvalType::Global },
        EvalFlags::empty(),
    );

    let mut promise_future = None;

    // If the result is a Promise, turn it into a Rust Future we can await.
    let res = ctx.with_root_scope(|mut cx| match result {
        Ok(val) => {
            if let Ok(promise) = Promise::try_from(val) {
                match promise.into_future(&mut cx) {
                    Ok(fut) => {
                        promise_future = Some(fut);
                        Ok(())
                    }
                    Err(_) => Err(cx.take_exception().to_text(&mut cx)),
                }
            } else {
                colorize_println(val.inspect(&mut cx).unwrap(), GreenFg);
                Ok(())
            }
        }
        Err(_) => Err(cx.take_exception().to_text(&mut cx)),
    });

    if let Err(e) = res {
        let mut err_msg = e.message;
        if let Some(tb) = e.traceback {
            err_msg = format!("{err_msg}\n{tb}");
        }
        colorize_println(err_msg, RedFg);
        return ExitCode::FAILURE;
    }

    // Drive the runtime and (optionally) await the Promise result concurrently.
    let run_result = if let Some(js_fut) = promise_future {
        let (rt_res, promise_res) = tokio::join!(rt.run_async(), js_fut);
        ctx.with_root_scope(|mut cx| match promise_res {
            Ok(val) => colorize_println(val.inspect(&mut cx).unwrap(), GreenFg),
            Err(reason) => colorize_println(
                reason.to_string(&mut cx).unwrap().encode_utf8(false).to_string(),
                RedFg,
            ),
        });
        rt_res
    } else {
        rt.run_async().await
    };

    // Map the runtime exit status to a process exit code.
    match run_result {
        ExitStatus::Value(_) => ExitCode::SUCCESS,
        ExitStatus::Interrupted(code) => code,
        ExitStatus::Exception(Exception::OutOfMemory) => {
            colorize_println("out of memory", RedFg);
            ExitCode::FAILURE
        }
        ExitStatus::Exception(Exception::Error(err)) => {
            ctx.with_root_scope(|mut cx| {
                colorize_println(err.to_string(&mut cx).unwrap().encode_utf8(false), RedFg);
            });
            ExitCode::FAILURE
        }
        _ => ExitCode::FAILURE,
    }
}
```

Key differences from the synchronous variant:

- `#[tokio::main]` makes the entry point async.
- `scope.eval()` is used instead of `scope.eval_wait()` — it does not block.
- `rt.run_async().await` drives the runtime’s tick loop as a `Future`.
- `Promise::into_future()` converts a JS Promise into a Rust `Future` that can
  be `await`ed directly.
