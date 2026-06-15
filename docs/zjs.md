# ZJS - Zuqe JavaScript Shell

ZJS is a command-line JavaScript interpreter built on top of the Zuqe engine. It provides a comprehensive runtime environment for executing JavaScript code with access to native system functionality.

## Usage

Suppose you have got `zjs` binary, use it like:

```bash
zjs [options] <filename>
```

### Options

- **`-m, --module`**: Load the script as an ES6 module. When this flag is set, the script will be parsed and executed using ES6 module semantics instead of global script execution.

- **`--memory-limit <n>`**: Set the memory (GC heap) limit to `n` bytes. Supports the same size suffixes as `--stack-size`.

- **`--stack-size <n>`**: Set the JavaScript virtual machine stack size to `n` bytes. The stack size can be specified with suffixes:
  - `k` or `K` for kilobytes (multiply by 2^10)
  - `m` or `M` for megabytes (multiply by 2^20)
  - `g` or `G` for gigabytes (multiply by 2^30)

  Examples:

  ```bash
  zjs --stack-size 1m script.js    # 1 megabyte stack
  zjs --stack-size 512k script.js # 512 kilobytes stack
  zjs --stack-size 2g script.js   # 2 gigabytes stack
  ```

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

# Set custom stack size
zjs --stack-size 2m my-script.js

# Execute with custom stack size and module mode
zjs --stack-size 1m -m esm-script.js
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
   - Implementation of built-in global functions like `print()` and `createRealm()`
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

// Configure stack size
rt.set_stack_size(stack_size_bytes);

// Setup module loader
rt.set_module_loader(None, js_module_loader, std::ptr::null_mut());

// Register custom classes
let class_id = CustomClass::register(&mut rt)?;

// Create execution context and set up environment
let mut ctx = rt.create_context(|cx| {
    // ECMA standard stuff
    ScopedContext::ECMA_CONTEXT_SETUP(cx)?;

    // customized stuff
    cx.use_class(class_id)?;
    init_modules(cx)?;
    setup_globals(cx)
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

    let mut promise_future = None;

    let res = ctx.with_new_scope(|mut scope| {
        let result = scope.eval(
            script.as_bytes(),
            &args.filename,
            if is_module { EvalType::Module } else { EvalType::Global },
            EvalFlags::empty(),
        );

        let r = match result {
            Ok(val) => {
                if let Ok(promise) = Promise::try_from(val) {
                    match promise.into_future(&mut scope) {
                        Ok(fut) => {
                            promise_future = Some(fut);
                            Ok(())
                        }
                        Err(_) => Err(scope.take_exception().to_text(&mut scope)),
                    }
                } else {
                    // Non-promise result: print directly
                    Ok(())
                }
            }
            Err(_) => Err(scope.take_exception().to_text(&mut scope)),
        };
        scope.return_with(r)
    });

    // If eval error, print and exit
    if let Err(e) = res {
        // print error
        return ExitCode::FAILURE;
    }

    if let Some(js_fut) = promise_future {
        let (rt_res, promise_res) = tokio::join!(rt.run_async(), js_fut);
        // Both the runtime and the JS Promise complete here
    } else {
        let _ = rt.run_async().await;
    }

    exit_code
}
```

Key differences from the synchronous variant:

- `#[tokio::main]` makes the entry point async.
- `scope.eval()` is used instead of `scope.eval_wait()` — it does not block.
- `rt.run_async().await` drives the runtime’s tick loop as a `Future`.
- `Promise::into_future()` converts a JS Promise into a Rust `Future` that can
  be `await`ed directly.
