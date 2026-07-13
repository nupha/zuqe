# Native Function

This tutorial explains how to define native functions: Rust functions that can be called from the JavaScript environment. Native functions bridge the gap between Rust and JavaScript, allowing you to expose Rust functionality to your scripts.

## 1. Function Signature

Native functions have a specific signature that the runtime expects:

```rust
use zuqe::{CallContext, JSResult, JSValue, ScopedContext};

fn my_native_function(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    // Function implementation
    Ok(JSValue::UNDEFINED)
}
```

The function must return `Result<JSValue, i32>` and receive:

- The current scoped context via `ctx: &mut ScopedContext`
- Function arguments via `cc.arg(index)` and `cc.arguments()`
- The `this` value via `cc.this()`
- Function metadata like `length` and `magic`

## 2. Arguments

Use the `CallContext` methods to access JavaScript arguments:

```rust
fn js_add_numbers(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    // Get first argument as i32
    let a = cc.arg(0).to_i32(ctx)?;

    // Get second argument as i32
    let b = cc.arg(1).to_i32(ctx)?;

    // Return sum as JavaScript number
    Ok(JSValue::from(a + b))
}
```

Common conversion methods:

- `to_i32(ctx)` - Convert to 32-bit integer
- `to_f64(ctx)` - Convert to float
- `to_string(ctx)` - Convert to JavaScript string
- `to_bool()` - Convert to boolean
- `to_index(ctx)` - Convert to array index

## 3. Working with `this` Value

Access the `this` value passed to the function:

```rust
fn js_get_this_property(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_value = cc.this();

    // Get property 'name' from 'this'
    let name = this_value.get_property(ctx, "name")?;
    Ok(name)
}
```

## 4. Creating Native Functions

Use `create_native_function()` on a scoped context to create a function object:

```rust
use zuqe::{Callback, ScopedContext};

let func = ctx.create_native_function(
    Callback::Func(js_add_numbers), // Function implementation
    "addNumbers",                   // Function name
    2,                             // Expected argument count
    0,                             // Magic value (for function overloading)
)?;
```

Parameters:

- `Callback::Func(function)` - Wraps your function implementation
- Function name exposed to JavaScript
- Minimum number of arguments expected
- Magic value for function variant identification

## 5. Function Overloading with Magic Values

Use magic values to create overloaded functions:

```rust
fn js_print(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let magic = cc.magic(); // Get magic number

    match magic {
        0 => println!("Normal print"),
        1 => println!("Debug print"),
        _ => println!("Unknown print type"),
    }

    Ok(JSValue::UNDEFINED)
}

// Create different variants
let print_normal = ctx.create_native_function(
    Callback::Func(js_print), "print", 0, 0)?;

let print_debug = ctx.create_native_function(
    Callback::Func(js_print), "print", 0, 1)?;
```

## 6. Error Handling

Throw JavaScript errors from native functions:

```rust
fn js_divide(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let a = cc.arg(0).to_f64(ctx)?;
    let b = cc.arg(1).to_f64(ctx)?;

    if b == 0.0 {
        ctx.throw_range_error("Division by zero");
        return Err(-1);
    }

    Ok(JSValue::from(a / b))
}
```

Common error methods:

- `ctx.throw_range_error(message)`
- `ctx.throw_type_error(message)`
- `ctx.throw_reference_error(message)`
- `ctx.throw_internal_error(message)`

## 7. Setting Global Functions

Attach functions directly to the global object:

```rust
pub fn setup_globals(ctx: &mut ScopedContext) -> Result<(), i32> {
    let mut global = ctx.global_obj();

    let print_func = ctx.create_native_function(
        Callback::Func(js_print), "print", 0, 0)?;

    global.set_property_str(ctx, "print", print_func.into())?;
    Ok(())
}
```

## 8. Constructor Functions

Create constructor functions that can be called with `new`:

```rust
fn js_person_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    // Get name from first argument
    let name = cc.arg(0).to_string(ctx)?;

    // Create new object with properties
    let mut obj = ctx.create_base_object()?;
    obj.set_property_str(ctx, "name", name.into())?;

    Ok(JSValue::Object(obj))
}

let constructor = ctx.create_native_function(
    Callback::Constructor(js_person_constructor),
    "Person",
    1,  // Expected arguments
    0)?;
```

## 9. Complete Example

Here's a complete example combining all concepts:

```rust
use zuqe::{CallContext, Callback, JSResult, JSValue, ScopedContext};

fn js_greet(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let name = cc.arg(0).to_string(ctx)?;
    let greeting = format!("Hello, {}!", name.content().encode_utf8(false));

    ctx.create_string_from_str(&greeting).map(JSValue::String)
}

fn js_calculate(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let operation = cc.arg(0).to_string(ctx)?;
    let a = cc.arg(1).to_f64(ctx)?;
    let b = cc.arg(2).to_f64(ctx)?;

    let result = match operation.content().encode_utf8(false).as_ref() {
        "add" => a + b,
        "subtract" => a - b,
        "multiply" => a * b,
        "divide" => {
            if b == 0.0 {
                ctx.throw_range_error("Division by zero");
                return Err(-1);
            }
            a / b
        }
        _ => {
            ctx.throw_type_error("Invalid operation");
            return Err(-1);
        }
    };

    Ok(JSValue::from(result))
}

// Register functions
pub fn init_utilities(ctx: &mut ScopedContext) -> Result<(), i32> {
    let mut global = ctx.global_obj();

    let greet_func = ctx.create_native_function(
        Callback::Func(js_greet), "greet", 1, 0)?;

    let calc_func = ctx.create_native_function(
        Callback::Func(js_calculate), "calculate", 3, 0)?;

    global.set_property_str(ctx, "greet", greet_func.into())?;
    global.set_property_str(ctx, "calculate", calc_func.into())?;

    Ok(())
}
```

Usage in JavaScript:

```javascript
// Call native functions
let message = greet("Alice"); // "Hello, Alice!"
let result = calculate("add", 10, 5); // 15
```

This tutorial covered the essentials of creating and using native functions in the Zuqe runtime. Native functions provide a powerful way to extend JavaScript with Rust capabilities, enabling you to build sophisticated applications that leverage both languages effectively.

## See Also

- [Native Module](native-module.md) - Learn how to export native functions as module exports
