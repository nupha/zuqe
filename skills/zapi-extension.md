# 编写 zapi 扩展（cdylib）

> 面向第三方扩展作者。zapi 扩展是动态库（Linux `.so` / macOS `.dylib` /
> Windows `.dll`），由宿主加载成一个 ES 模块。**不要**把 zapi 的 `Zq*` ABI
> 类型与宿主内建的 `ScopedContext`/`FieldDef`/`def_native_object!` 混用。
>
> 实现细节与 ABI 见 `docs/zapi-extension.md`、`src/zapi/host.rs`。
> 仓库内可运行示例：`examples/zapi_basic`、`examples/zapi_event`、
> `examples/zapi_raw`。

## 1. 工程结构

```toml
[package]
name = "mymod"
version = "0.1.0"
edition = "2024"

[lib]
name = "mymod"                 # 生成 libmymod.so / libmymod.dylib / mymod.dll
crate-type = ["cdylib"]        # 必须是动态库

[dependencies]
zuqe_zapi = "0.4.0"
zuqe_zapi_macros = "0.4.0"
```

```rust
use zuqe_zapi::*;
use zuqe_zapi_macros::{zq_function, zq_module};
```

- `crate-type = ["cdylib"]` 不能漏；否则宿主没有东西可 `dlopen`。
- `[lib] name` 决定导入的文件名，属于公开 API，保持稳定。
- 扩展与宿主的 `zapi` ABI 版本必须匹配；不匹配时 import 报
  `zapi ABI/API version N mismatch`。

## 2. 最小模块

```rust
#[zq_function]
fn add<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let a = cc.arg(0).to_f64(ctx)?;
    let b = cc.arg(1).to_f64(ctx)?;
    Ok(ZqValue::number(a + b))
}

#[zq_function(name = "greet")]
fn greet<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let name = cc.arg(0).coerce_string(ctx)?;
    ZqValue::new_string(ctx, &format!("hello, {name}"))
}

#[zq_module(name = "mymod", exports = [add, greet])]
fn module_init() {}
```

- `#[zq_module]` 生成 `zq_module_register`，宿主 `dlopen` 后调用。
- `exports` 是显式的 Rust 标识符列表；函数 JS 名默认等于 Rust 名，可用
  `#[zq_function(name = "...")]` 改。类用 `#[zq_class(name = "...")]` 的 JS 名。
- `#[zq_module]` 标注的函数就是 init hook，加载时运行一次；两种签名：
  `fn module_init()` 或 `fn module_init(ctx: &ZqCtx) -> Result<(), ZqError>`。
- 可选 `finalize = my_fn`：每个 runtime 销毁时调用一次，**不能调用任何 JS API**。

## 3. 参数、返回值与错误

```rust
cc.arg(0).to_f64(ctx)?        // ToNumber
cc.arg(1).to_i32(ctx)?
cc.arg(2).to_bool(ctx)?       // ToBoolean
cc.arg(3).coerce_string(ctx)? // ToString → String
cc.arg(4).string_read_utf8(ctx)? // 严格字符串读取；非字符串返回 TypeError
cc.arg_f64(ctx, 0)?           // 便捷别名
cc.len()                      // 实参个数
cc.this()                     // this
cc.magic()                    // #[zq_function(magic = N)] / #[zq_method(magic = N)]
cc.arg(0).kind()              // ZqValueType::{Undefined,Null,Bool,Number,String,Symbol,Function,Object,BigInt,Exception}
```

构造返回值：

```rust
ZqValue::int32(1)
ZqValue::number(1.5)
ZqValue::bool(true)
ZqValue::null()
ZqValue::undefined()
ZqValue::new_string(ctx, "hi")?
ZqValue::new_object(ctx)?
ZqValue::new_array(ctx, &[ZqValue::int32(1), ZqValue::bool(true)])?
```

`ZqValue::new_*` 返回 `Result<ZqValue, ZqError>`；当它是函数最后一个表达式时
直接返回，不要再包 `Ok` 或 `?`。

抛 JS 异常：

```rust
return Err(ctx.throw_type("`path` is required"));    // TypeError
return Err(ctx.throw_range("expected 2 arguments")); // RangeError
return Err(ctx.throw_reference("missing binding"));
```

`ctx.throw_*` 返回 `ZqError`，用 `Err(...)` 传播；`?` 也会把底层错误转成
JS 异常。

对象/数组/调用 JS：

```rust
let obj = ZqValue::new_object(ctx)?;
obj.set_property_str(ctx, "key", val)?;   // obj.key = val
obj.get_property_str(ctx, "key")?;        // 缺失返回 undefined
obj.has_property_str(ctx, "key")?;
obj.del_property_str(ctx, "key")?;
obj.set_property(ctx, 0i32, val)?;

let arr = ZqValue::new_array(ctx, &[])?;
arr.push(ctx, ZqValue::int32(1))?;
arr.length(ctx)?;
arr.get_property(ctx, 0i32)?;

let json = ctx.get_global_str("JSON")?;
let stringify = json.get_property_str(ctx, "stringify")?;
let text = ctx.call_function(stringify, json, &[obj])?;
// 或 callee.call(ctx, this, &args)；构造用 ctx.call_constructor(callee, &args)
```

旧别名 `set_str`/`get_str`/`has_str`/`delete_str`/`set`/`get` 已 deprecated。

## 4. 模块级值：`#[zq_export]`

```rust
use zuqe_zapi_macros::zq_export;

#[zq_export(name = "VERSION")]
fn version<'a>(ctx: ZqCtx<'a>) -> Result<ZqValue<'a>, ZqError> {
    ZqValue::new_string(ctx, "0.1.0")
}

#[zq_module(name = "mymod", exports = [add, version])]
fn module_init() {}
```

body 在模块求值时运行，可构造任意 JS 值（字符串、对象、数组、函数）。

## 5. Native class

```rust
use zuqe_zapi_macros::{zq_class, zq_class_impl, zq_method, zq_static_method, ZqGcTrace};

#[derive(ZqGcTrace)]
struct CounterPayload {
    count: i32,
    label: ZqRawValue,   // JS handle，必须被 trace
}

#[zq_class(name = "Counter", payload = CounterPayload)]
struct JsCounter;

#[zq_class_impl]
impl JsCounter {
    // 按名字识别为构造函数；new Counter(label)
    fn constructor<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let label = cc.arg(0).raw();
        JsCounter::new_instance(ctx, CounterPayload { count: 0, label })
    }

    #[zq_method]
    fn increment<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let p = JsCounter::payload_mut(ctx, cc.this())
            .ok_or_else(|| ctx.throw_type("not a Counter"))?;
        p.count += 1;
        Ok(ZqValue::int32(p.count))
    }

    #[zq_method(name = "count", get)]
    fn count<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let p = JsCounter::payload(ctx, cc.this())
            .ok_or_else(|| ctx.throw_type("not a Counter"))?;
        Ok(ZqValue::int32(p.count))
    }

    #[zq_static_method(name = "isCounter")]
    fn is_counter<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        Ok(ZqValue::bool(JsCounter::payload(ctx, cc.arg(0)).is_some()))
    }
}
```

- `#[zq_method]` 装到 prototype；`#[zq_static_method]` 装到 constructor；
  `get`/`set` 变成属性。
- 每个方法都要用 `payload`/`payload_mut` 校验 `this`，失败抛 `TypeError`。
- 返回 `cc.this()` 支持链式调用；返回其他类实例用
  `OtherClass::new_instance(ctx, payload)`。
- `magic = N` + `cc.magic()` 用一个 body 服务多个 JS 名字。
- `extends = Base` 只能指向**同一个扩展里**的 `#[zq_class]`；registration
  顺序无关，但跨 `.dylib` 或内建类（如 `Array`）不能作为父类。父类关系是
  member lookup，不是 `super`；子类 payload 独立。
- **GC**：payload 里每个 `ZqRawValue`/`ZqValue` 都必须可 trace。
  用 `#[derive(ZqGcTrace)]` 覆盖所有字段；字段都不含 JS 句柄时用
  `payload_no_gc` 跳过 trace。`payload_no_gc` + 实际持有 `ZqRawValue` 会造成
  UAF。tuple/newtype payload 需要手写 `impl ZqGcTrace`。
- 类也要列进 `#[zq_module(exports = [...])]`。

## 6. 异步与后台工作

JS 在单线程上运行；慢操作必须移出 JS 线程。

### 6.1 `promise_threaded`：一次性线程任务

```rust
#[zq_function(name = "doubleAsync")]
fn double_async<'a>(ctx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let n = cc.arg(0).to_f64(ctx)?;
    ctx.promise_threaded(
        move || n * 2.0,                            // 在独立 OS 线程运行
        |ctx, result| Ok(ZqValue::number(result)),  // 回到 JS 线程
    )
}
// JS: await my.doubleAsync(21); // 42
```

`work` 必须 `Send + 'static`，且**不能**捕获/返回 `ZqRawValue`/`ZqValue`
等 GC 句柄；只捕获普通 Rust 数据（`f64`、`String`、`Vec<u8>`、`Arc<Mutex<_>>`）。
`convert` 在 JS 线程执行一次，`Err(ctx.throw_type(...))` 会 reject Promise。

### 6.2 `promise_fut`：运行在已注册的 executor

```rust
// executor 是 plain fn（不能捕获宿主状态）；在 module init 里注册一次。
// fn spawn_on_host_executor(fut: Pin<Box<dyn Future<Output = ()> + Send + 'static>>)
ctx.set_default_executor(spawn_on_host_executor);

let p = ctx.promise_fut(async move { do_io().await }, |ctx, result| {
    Ok(ZqValue::new_string(ctx, &result)?)
});
```

`promise_fut` 需要先 `set_default_executor`；否则返回 internal error。
需要捕获某个 tokio handle 时用 `promise_fut_on(fut, spawn_fn, convert)`，它的
`spawn_fn` 是 `FnOnce`，可以捕获 handle：

```rust
let p = ctx.promise_fut_on(
    async move { do_io().await },
    |fut| { tokio_handle.spawn(fut); },
    |ctx, result| Ok(ZqValue::new_string(ctx, &result)?),
);
```

### 6.3 Async iterator

```rust
struct Rows { done: bool }

impl ZqAsyncIterable for Rows {
    fn next<'a>(&mut self, ctx: ZqCtx<'a>) -> Result<AsyncIterResult<'a>, ZqError> {
        if self.done { return Ok(AsyncIterResult::Done); }
        match fetch_next_row(ctx)? {
            Some(v) => Ok(AsyncIterResult::Value(build_row(ctx, v)?)),
            None => { self.done = true; Ok(AsyncIterResult::Done) }
        }
    }
}

ctx.new_async_iterator(Rows { done: false })
```

`AsyncIterResult::Wait` 表示“现在没有，但下个 tick 再试”，不会阻塞 JS。

### 6.4 轮询 job：`add_job` / `add_job_no_gc`

```rust
let (tx, rx) = ctx.make_wake_channel::<i32>();

std::thread::spawn(move || {
    for i in 0..10 { let _ = tx.send(i); } // send/drop 自动唤醒事件循环
});

ctx.add_job_no_gc(move |_cx| match rx.try_recv() {
    Ok(v) => { println!("stream: {v}"); ZqPoll::Again }
    Err(std::sync::mpsc::TryRecvError::Empty) => ZqPoll::Again,
    Err(std::sync::mpsc::TryRecvError::Disconnected) => ZqPoll::Done,
});
```

- `ZqPoll::Done` 结束；`Again` 保持注册；`AgainNoHold` 不保活进程。
- `add_job_no_gc` 的闭包不得捕获任何 `ZqValue`/`ZqRawValue`。需要捕获 JS 值
  （例如每收到一个值就调用 JS 回调）时，用 `add_job` 并实现 `ZqJob`
  （`run` + `ZqGcTrace::trace`），把回调放进被 trace 的 payload。
- `ZqWakeSender` 是 `Send + Sync`，每个 clone 共享同一个唤醒令牌；drop 也会
  唤醒，避免消费者永远 park。

## 7. 宿主加载

宿主必须：

1. 用 `--features zapi` 构建；
2. 安装能识别 `.so`/`.dylib`/`.dll` 的 `Runtime::set_module_loader`；
3. 调用 `ScopedContext::load_zapi_extension(path)`（zjs 的 loader 已这样做）。

```js
import * as my from "./libmymod.so";
my.add(2, 3); // 5
```

没有 loader 就没有 `import`。扩展库由宿主进程级缓存并保持映射，不卸载，
避免导出函数指针悬垂。

## 8. 提交前 checklist

- [ ] `crate-type = ["cdylib"]`，`[lib] name` 稳定。
- [ ] 所有公开成员都在 `#[zq_module(exports = [...])]` 中。
- [ ] 每个类方法都用 `payload`/`payload_mut` 校验 `this`。
- [ ] 含 JS 句柄的 payload 有 `ZqGcTrace`；`payload_no_gc` 只用于零 JS 句柄。
- [ ] 慢工作走 `promise_threaded`/`promise_fut`/async iterator，不阻塞 JS 线程。
- [ ] 错误用 `ctx.throw_*` + `Err`，不要吞异常。
- [ ] 扩展与宿主 zapi 版本匹配。
