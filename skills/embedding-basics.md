# 嵌入基础：Runtime、Context 与执行

> 面向宿主 Rust 代码（直接依赖 `zuqe`）。这里只讲公开 API 的“怎么用”；
> tick / Promise job / GC 的内部实现见 `docs/loop.md`、`docs/promise.md`、`docs/gc.md`。

## 1. Cargo 依赖与 feature

```toml
[dependencies]
zuqe = "0.11"          # 以 Cargo.toml 中的实际版本为准
tokio = { version = "1", features = ["full"] } # 仅异步宿主需要
```

默认 feature 集合（`Cargo.toml` 的 `default`）包含：
`inline_cache`、`big_int`、`all_unicode`、`async_support`、`gc_generations`、
`jit`，以及 `builtin_mapset/proxy/atomic/regexp/reflect/json/date/math/weakref`。

常用可选 feature：

| feature | 作用 |
|---|---|
| `zapi` | 加载 zapi 动态扩展（`cdylib`），见 [zapi-extension.md](zapi-extension.md) |
| `intl402` | `Intl` 支持，依赖 ICU |
| `persistent` | 字节码持久化（`zjs` 的 `dumpValue`/`restoreValue`） |
| `crossbeam_channel` | 用 `crossbeam::channel` 替代 `std::sync::mpsc` |
| `serde_json_support` | `JSValue` ↔ `serde_json::Value` |
| `jit_dump` | 转储 Cranelift IR / 机器码，仅调试 |
| `gc_protect_transient` | 仅在 `gc_alloc` 可能触发 GC 时需要；一般不要开 |

裁剪构建用 `--no-default-features --features "...`，逐个列出真正需要的功能。

## 2. 创建 Runtime 与 Context

`Runtime` 拥有 GC 堆和事件循环；`Context` 是隔离的全局环境。一个 Context
只属于一个 Runtime。

```rust
use zuqe::{Context, Runtime, ScopedContext};

fn build() -> Result<(Runtime, Context<'static>), i32> {
    let mut rt = Runtime::create()?;
    let ctx = rt.create_context(|cx| ScopedContext::ECMA_CONTEXT_SETUP(cx))?;
    Ok((rt, ctx))
}
```

`ScopedContext::ECMA_CONTEXT_SETUP` 安装标准 ECMAScript 全局对象；也可以传入
自己的 setup 闭包，只暴露受限的宿主 API。

## 3. 执行脚本：`eval_wait` 与 `eval`

### 3.1 推荐：`eval_wait`（驱动到事件循环空闲）

`Context::eval_wait` 负责执行入口脚本/模块，并驱动 tick 直到没有待处理任务。
默认 `async_support` 打开时它是 **async** 函数，参数里**没有** `&mut Runtime`：

```rust
use zuqe::{EvalFlags, EvalName, EvalType, Exception, ExitStatus};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let mut rt = zuqe::Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(|cx| zuqe::ScopedContext::ECMA_CONTEXT_SETUP(cx))
        .unwrap();

    let status = ctx
        .eval_wait(
            b"Promise.resolve(42)",
            &EvalName::Name("<host>".into()),
            EvalType::Global,
            EvalFlags::empty(),
        )
        .await;

    match status {
        ExitStatus::Value(v) => {
            let s = ctx.with_root_scope(|mut cx| {
                v.to_string(&mut cx).unwrap().content().encode_utf8(false)
            });
            println!("{s}");
            std::process::ExitCode::SUCCESS
        }
        ExitStatus::Interrupted(code) => code,
        ExitStatus::Exception(Exception::Error(err)) => {
            ctx.with_root_scope(|mut cx| {
                eprintln!("{}", err.to_string(&mut cx).unwrap().content().encode_utf8(false));
            });
            std::process::ExitCode::FAILURE
        }
        ExitStatus::Exception(_) => std::process::ExitCode::FAILURE,
    }
}
```

`eval_wait` 会观察入口的顶层 Promise：模块顶层 rejection 会变成
`ExitStatus::Exception(Exception::Error(reason))`，不会被静默丢弃。

当 `async_support` 关闭时，`Context::eval_wait` 与 `ScopedContext::eval_wait`
是同步函数，签名中同样没有 Runtime：

```rust
let status = ctx.with_root_scope(|mut scope| {
    scope.eval_wait(
        b"1 + 1",
        &zuqe::EvalName::Name("<host>".into()),
        zuqe::EvalType::Global,
        zuqe::EvalFlags::empty(),
    )
});
```

### 3.2 低层：`eval` + `run_async`

`eval` 不阻塞：普通值立即返回；如果脚本返回 Promise，需要自己把它转成
Future，并与 `Runtime::run_async()` 一起驱动。

```rust
use zuqe::builtins::promise::Promise;
use zuqe::{EvalFlags, EvalName, EvalType};

let mut promise_future = None;
let mut direct = None;

ctx.with_root_scope(|mut scope| {
    let val = scope.eval(
        b"Promise.resolve(7)",
        &EvalName::Name("<host>".into()),
        EvalType::Global,
        EvalFlags::empty(),
    )?;
    if let Ok(p) = Promise::try_from(val) {
        promise_future = Some(p.into_future(&mut scope)?);
    } else {
        direct = Some(val);
    }
    Ok::<_, i32>(())
})?;

let value = if let Some(fut) = promise_future {
    let (_run, result) = tokio::join!(rt.run_async(), fut);
    result.unwrap_or_else(|reason| reason)
} else {
    direct.unwrap_or(zuqe::JSValue::UNDEFINED)
};
```

`Runtime::run_async()` 返回 `RunFuture`，其 `Output` 是 `ExitStatus`。更多
Promise 桥接见 [promise.md](promise.md)。

## 4. `EvalName` / `EvalType` / `EvalFlags`

- `EvalName::Name(s)`：只作为调试文件名，不绑定 URL base；`Name("")` 表示显式无名字。
- `EvalName::BaseUrl(url)`：同时作为名字和 URL base。**模块执行必须用它**，
  这样 `import.meta.url` 和相对 `import()` 才能解析。
- `EvalType::Global`：脚本语义；`EvalType::Module`：ESM，返回 Promise；
  `EvalType::Indirect`：间接 eval 语义；`Direct` 只用于内部 eval。
- `EvalFlags::STRICT`、`STRIP`、`BACKTRACE_BARRIER`、`COMPILE_TO_ASYNC`、
  `NO_AWAIT_RESULT`。一般宿主入口用 `EvalFlags::empty()`。

入口文件名转 URL 用 `zuqe::path_to_url`（返回 `Result<Url, _>`）。

## 5. GC scope 的公开入口

- `Context::with_root_scope(|scope| ...)`：在 root scope 中执行。root scope
  中的值会一直存活到 Context 销毁，不需要时用 `scope.remove_local(node)`。
- `ScopedContext::with_new_scope(|scope| ...)`：创建子 scope；闭包返回值会
  promotion 到父 scope。临时工作放这里。
- `Context::eval` / `Context::eval_wait` 内部已经创建子 scope，调用方不需要
  再手动包一层。

`Context::with_new_scope` 是 crate 内部方法；宿主应写成
`ctx.with_root_scope(|scope| scope.with_new_scope(...))`。

## 6. 资源限制与宿主配置

```rust
let mut rt = zuqe::Runtime::create()?;
rt.install_panic_handler();                 // release 下 panic 时尽量回收 GC

rt.gc_set_memory_limit(64 << 20);           // GC 堆上限（字节）
rt.set_vm_stack_arena(256 << 10).map_err(|_| -1)?; // VM 帧 bump arena
rt.set_vm_stack_limit(512 << 10);           // VM 虚拟栈总上限（arena + heap spill）
rt.set_thread_stack_limit(512 << 10);       // C 栈递归上限；0 = 自动
```

- `set_vm_stack_arena` 返回 `Result<(), ()>`，分配失败时返回 `Err(())`。
- `set_vm_stack_limit` 应 `>=` arena；小于 arena 只会浪费 arena 空间。
- `set_thread_stack_limit(0)` 为自动模式，从当前线程真实栈底推导并保留
  `THREAD_STACK_LIMIT_MARGIN` 的 throw 路径余量。
- C-stack 自动模式依赖平台能报告栈底；`Runtime::CAN_AUTO_THREAD_STACK_LIMIT`
  给出编译期判断。

## 7. 模块加载钩子

模块来源是宿主策略：

```rust
rt.set_module_normalizer(my_normalizer); // 可选，默认归一化已可用
rt.set_module_loader(my_loader);         // 不设置则每次 import 抛 ReferenceError
```

两个回调都是普通 `fn` 指针，不带宿主状态；需要状态时放
`RuntimeImpl::user_state`，回调里通过 `ctx.rt()` 访问。完整签名、import
attributes、`import.meta` 与 zapi 扩展加载见 [native-module.md](native-module.md)。

## 8. 最小检查清单

- [ ] Runtime 与 Context 的创建顺序正确；`Context` 不跨线程、不跨 Runtime。
- [ ] 默认 `async_support` 下使用 `Context::eval_wait(...).await`，没有多传
      `&mut Runtime`。
- [ ] 模块入口用 `EvalName::BaseUrl(path_to_url(...)?)`。
- [ ] Promise 返回值用 `Promise::try_from` + `into_future`，并与 `run_async`
      一起驱动。
- [ ] 临时值放 `with_new_scope`；root scope 中的值及时 `remove_local`。
- [ ] 资源限制在创建 Context 前后按需设置；`set_vm_stack_arena` 的错误已处理。
