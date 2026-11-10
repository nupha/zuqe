# 绑定本地函数（native / host function binding）

> 适用对象：**宿主/引擎内建** Rust 代码，直接依赖 `zuqe`。
> **不适用于 zapi 第三方 cdylib 扩展**：zapi 使用 `ZqBodyFn`、`ZqRawValue`/`ZqRawCtx`、`ZqFieldDef` 与 trampoline 机制，属于独立 ABI 路径（见 [native-module.md](native-module.md) 的 zapi 边界）。不要混用。

## 核心类型与签名

```rust
use zuqe::{CallContext, JSResult, JSValue, ScopedContext, Callback};

// 宿主/内建原生回调签名（function/fields.rs / nfun.rs）
pub type CallbackFn = fn(ctx: &mut ScopedContext, cc: CallContext) -> JSResult;
// JSResult = Result<JSValue, ErrCode>（error.rs），惯例上抛异常后返回 Err(-1)
```

`CallContext` 常用方法（`src/function/call.rs`）：
- `cc.this()`：`this` 值
- `cc.arguments()` / `cc.arg(n)`：参数切片 / 第 n 个参数（越界返回 `UNDEFINED`）
- `cc.length()`：实参个数
- `cc.magic()`：`FieldDef::FUNC` 注册时的 magic（用于多函数复用同一回调）
- `cc.data(n)` / `cc.data_len()`：绑定数据（`create_native_function_with_data` 场景）
- `cc.replace_data(ctx, n, val)`：替换第 n 个绑定数据；内部完成 store + write barrier
  （`NFuncPayload.data` 是私有字段，这是唯一后写路径）

## 参数、返回值与错误处理

- **读取参数**：用 `cc.arg(n)`/`cc.arguments()` 取得 `JSValue`。`JSValue` 没有
  `to_int32`；数值转换用 `to_number(ctx)?` 得到 `IntOrFloat`，再
  `.to_i32()` / `.to_f64()` / `.to_u32()`。其他转换：`to_bool()?`、
  `to_string(ctx)?`、`to_index(ctx)?`（`i64`，用于数组索引）。
- **返回值**：成功返回 `Ok(value)`；无返回值用 `Ok(JSValue::UNDEFINED)`。
- **抛异常**：通过 `ScopedContext` 的 `throw_*`（如 `throw_type_error`、`throw_range_error`、
  `throw_internal_error` 等）设置当前异常，然后 `return Err(-1)`。**不要**在已设置异常时
  再返回 `Ok(...)`。
- **`this` 校验**：方法通常需校验 `cc.this()` 是否为期望的 native object/class 实例；
  类型不匹配时抛 `TypeError`。

## 注册方式（宿主/内建）

### 1) 通过 `FieldDef` 表注册（类原型/构造器/模块导出常用）

`FieldDef`（`src/function/fields.rs`）用于声明式字段/方法表，供 `def_native_object!` 的 proto/ctor entries、`ClassDef`、`ScopedContext::create_module` 等消费：

```rust
use zuqe::{CallContext, FieldDef, JSResult, JSValue, ScopedContext, JS_PROP_C_W_E};

fn js_foo_bar(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let _this = cc.this();
    let arg0 = cc.arg(0);
    // ... 校验/转换/处理 ...
    Ok(JSValue::UNDEFINED)
}

pub const FOO_PROTO_FUNCS: &[FieldDef] = &[
    FieldDef::FUNC("bar", 1, js_foo_bar, 0), // (name, length, callback, magic)
    // FieldDef::GETSET / GETSET_ENUM / STR / INT32 / ... 见 fields.rs
];
```

`FieldDef::FUNC` 的 `length` 是 JS 可见形参个数（`u8`）；`magic` 是 `i16`，
可用于一个回调分发多个操作（`cc.magic()`）。

### 2) 创建独立 native function 对象并挂到对象/全局

当需要在运行时动态创建函数并安装到某个对象（如全局对象、`import.meta` 扩展、模块命名空间辅助）时，使用 `ScopedContext` 的函数创建 API（`src/function/nfun.rs`）：

```rust
use zuqe::{CallContext, Callback, JSResult, JSValue, ScopedContext, JS_PROP_C_W_E};

fn host_hello(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let name = cc.arg(0);
    // ... 将 name 转为字符串并处理；示意省略转换细节
    let _ = name;
    Ok(JSValue::UNDEFINED)
}

pub fn install_global(ctx: &mut ScopedContext) -> Result<(), i32> {
    let f = ctx.create_native_function(Callback::Func(host_hello), "hello", 1, 0)?;
    let mut g = ctx.global_obj(); // Object（ContextImpl::global_obj）
    g.define_property_value_str(ctx, "hello", JSValue::from(f), JS_PROP_C_W_E)?;
    Ok(())
}
```

> 具体挂载 API 以当前 `Object`/`ScopedContext` 方法为准（如 `define_property_value_str`、属性标志常量）。属性标志选择要符合语义（ECMA 内建通常 `C_W_E` 组合按需；WebIDL/宿主对象可见性按规范）。

### 3) 模块导出函数

native module 通过 `create_module` + `FieldDef` 导出；见 [native-module.md](native-module.md)。模块导出与“注册全局函数”是不同集成点，不要混淆。

### 4) 构造函数

需要 `new` 的原生类优先用 [native-class.md](native-class.md) 的
`def_native_object!` + `ClassCtorDef`：宏负责 prototype、constructor flag、
`new_instance` 和 payload。直接 `create_native_function(Callback::Constructor(..), ...)`
只会把函数标记为 constructor；实例创建、prototype 链接仍需按 class/引擎原语处理，
不建议宿主手写。`ClassCtorDef::new(cb, length, call_as_func)` 的第三个参数是
“能否不带 `new` 调用”，不是“是否 constructor”。

## `ScopedContext` 使用要点

> scope/root 与跨 JS 保活的规则见 [`gc-safety.md`](gc-safety.md#rule-4--值离开-traced-容器后跨-js-re-entry) Rule 4；trace 实现示例见
> [`gc-trace.md`](gc-trace.md#23-跨-js-调用前保活对应-rule-4)。

- `ScopedContext` 表示 GC scope：`with_root_scope` / `with_new_scope` 管理临时保护与返回值 promotion。在 native 回调中，`ctx` 已是当前作用域上下文；分配临时对象并返回时注意 promotion 规则（`ScopeResult`）与 scope 边界。
- 跨 JS re-entry 的值按 Rule 4 处理；实现示例见 [`gc-trace.md`](gc-trace.md#23-跨-js-调用前保活对应-rule-4)。

## 最小可运行模板（宿主内建函数）

```rust
use zuqe::{
    CallContext, Callback, EvalFlags, EvalName, EvalType, JSResult, JSValue, Runtime,
    ScopedContext, JS_PROP_C_W_E,
};

fn js_add1(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let n = cc.arg(0).to_number(ctx)?.to_i32();
    Ok(JSValue::from(n + 1))
}

fn main() -> Result<(), i32> {
    let mut rt = Runtime::create()?;
    let mut ctx = rt.create_context(|cx| {
        ScopedContext::ECMA_CONTEXT_SETUP(cx)?;
        Ok(())
    })?;

    ctx.with_root_scope(|mut root| {
        root.with_new_scope(|cx| {
            let f = cx.create_native_function(Callback::Func(js_add1), "add1", 1, 0)?;
            let mut g = cx.global_obj();
            g.define_property_value_str(cx, "add1", f.into(), JS_PROP_C_W_E)?;

            let v = cx.eval(
                b"add1(41)",
                &EvalName::Name("<host>".into()),
                EvalType::Global,
                EvalFlags::empty(),
            )?;
            assert!(v.is_number());
            Ok::<(), i32>(())
        })
    })
}
```

> 签名以当前源码为准：`CallbackFn = fn(&mut ScopedContext, CallContext) -> JSResult`，
> `JSResult = Result<JSValue, i32>`。模板展示**回调签名 + 异常返回 + 安装 + scope**
> 的标准形状。

## 反模式

- 在宿主/内建路径里使用 `ZqBodyFn`/`ZqRawValue`/zapi 注册 API，或反过来在 zapi 扩展里直接操作 `ScopedContext` 内部 Rust API（除非经 zapi host/约定允许且你明确知道边界）。
- 抛了异常却返回 `Ok(...)`；或吞掉错误不设置异常直接 `Err(-1)`（导致“无异常的失败”）。
- 在回调中跨 JS 调用后继续使用已从 traced 容器移出的裸 `JSValue/Object`，且未按 Rule 4 保活。
