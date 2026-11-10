# Promise 与 Rust Future 桥接

> 面向宿主 Rust 代码。JS 侧 Promise 语义（微任务、reaction 顺序、错误处理）
> 由引擎保证；这里讲宿主如何创建、settle、等待 Promise。
> Promise 的 job/tick 内部实现见 `docs/promise.md` 与 `docs/loop.md`。

```rust
use zuqe::builtins::promise::Promise;
```

## 1. 类型与状态

```rust
pub enum PromiseState { Pending, Fulfilled, Rejected }

impl Promise {
    pub fn state(&self) -> PromiseState;
    pub fn result(&self) -> JSValue;
}
```

`state()`/`result()` 主要用于宿主测试与诊断，不暴露给 JS。

## 2. 从 Rust 创建并 settle

```rust
impl Promise {
    /// 返回 pending Promise 和 [resolve, reject] 两个 JS 函数。
    pub fn create(ctx: &mut ScopedContext) -> Result<(Promise, [JSValue; 2]), i32>;

    pub fn resolve(&mut self, ctx: &mut ScopedContext, value: JSValue);
    pub fn reject(&mut self, ctx: &mut ScopedContext, value: JSValue);
    pub fn then(&mut self, ctx: &mut ScopedContext, success: JSValue, fail: JSValue)
        -> Result<Object, i32>;
}
```

```rust
let (mut promise, resolving) = Promise::create(ctx)?;
let _ = resolving; // 之后可调用它们，或直接：
promise.resolve(ctx, JSValue::from(42));
```

`resolve`/`reject` 会触发已注册的 reaction；`then` 遵循 `SpeciesConstructor`
语义，返回新的 wrapper Promise。

## 3. 由宿主任务驱动 Promise

### 3.1 `create_with_job`：在 JS 线程轮询

```rust
impl Promise {
    pub fn create_with_job(
        ctx: &mut ScopedContext,
        job: impl Fn(&mut ScopedContext) -> Result<Option<JSValue>, i32> + 'static,
    ) -> Result<Promise, i32>;
}
```

job 在 JS 线程的微任务循环中调用：

- `Ok(Some(v))`：resolve 并结束 job。
- `Ok(None)`：继续等待（job 保持注册；没有外部唤醒时事件循环会 park）。
- `Err(code)`：job 失败；调用方应已在 `ctx` 上设置异常，Promise 被 reject。

### 3.2 通道：`create_with_channel` / `create_with_receiver`

```rust
impl Promise {
    pub fn create_with_channel<D: Send + 'static>(
        ctx: &mut ScopedContext,
        convert: impl FnOnce(&mut ScopedContext, D) -> JSResult + 'static,
    ) -> Result<(Self, WakeSender<D>), i32>;

    pub fn create_with_receiver<D: Send + 'static>(
        ctx: &mut ScopedContext,
        rx: WakeReceiver<D>,
        convert: impl FnOnce(&mut ScopedContext, D) -> JSResult + 'static,
    ) -> Result<Self, i32>;
}
```

- 返回/接收的 `WakeSender<D>`、`WakeReceiver<D>` 是公开 newtype；底层
  `Sender`/`Receiver` 别名不是公开 API。
- `WakeSender` 在每次成功 `send`/`try_send` 以及 drop 时唤醒 park 的事件循环。
- `convert` 在 JS 线程调用一次，返回 `JSResult` 决定 fulfill/reject。
- channel 在收到值前断开时，Promise 以 internal error reject。
- 需要新通道用 `ctx.make_channel(n)`（有界）或 `ctx.make_local_channel()`（无界，
  `D` 不必 `Send`）；已有 `WakeReceiver` 时用 `create_with_receiver`。

```rust
let (promise, tx) = Promise::create_with_channel(ctx, |cx, res: Result<Vec<u8>, String>| {
    match res {
        Ok(bytes) => Ok(JSValue::from(cx.create_base_object()?)),
        Err(e) => { cx.throw_internal_error(&e); Err(-1) }
    }
})?;

std::thread::spawn(move || {
    let _ = tx.send(do_blocking_io());
});
// JS: const data = await host.readFile(...);
```

### 3.3 `create_with_result_slot`：单次写入槽

```rust
pub struct PromiseResultSlot<D: Send> { /* ... */ }

impl<D: Send + 'static> PromiseResultSlot<D> {
    pub fn new() -> Self;
    pub fn is_empty(&self) -> bool;
    pub fn commit(&self, data: D) -> Result<(), D>;
}

impl Promise {
    pub fn create_with_result_slot<D: Send + 'static>(
        ctx: &mut ScopedContext,
        slot: PromiseResultSlot<D>,
        convert: impl FnOnce(&mut ScopedContext, D) -> JSResult + 'static,
    ) -> Result<Self, i32>;
}
```

- `commit` 线程安全；槽只能写一次，第二次返回 `Err(data)`。
- 传给 `create_with_result_slot` 时槽必须为空，否则抛 internal error。
- 所有 committer 在未提交的情况下 drop 时，debug 立即 panic，release reject，
  不会永久挂起。

### 3.4 `hold_when_awaited` 变体

`create_with_receiver_hold_when_awaited` 和
`create_with_channel_hold_when_awaited` 只在 Promise 被观察（`await`/`.then`）
时维持事件循环；未观察的 fire-and-forget Promise 不会阻止进程退出。

## 4. Promise ↔ Future

### 4.1 Promise → Future：`into_future`

```rust
impl TryFrom<JSValue> for Promise { type Error = i32; }

impl Promise {
    pub fn into_future(self, ctx: &mut ScopedContext) -> Result<PromiseFuture, i32>;
}
```

```rust
let val = scope.eval(code, &EvalName::Name("<test>".into()), EvalType::Global, EvalFlags::empty())?;
if let Ok(p) = Promise::try_from(val) {
    let fut = p.into_future(scope)?;
    let result: Result<JSValue, JSValue> = fut.await?;
}
```

`PromiseFuture` 实现 `Future<Output = Result<JSValue, JSValue>>`；内部挂
`then`/`catch`，settle 时唤醒 Waker。

### 4.2 Future → Promise：`create_from_future`

```rust
impl Promise {
    pub fn create_from_future<F>(ctx: &mut ScopedContext, fut: F) -> Result<Self, i32>
    where
        F: FnOnce(Box<dyn FnOnce(Result<JSValue, JSValue>) + 'static>);
}
```

这个 API 不接收 `Future` 本身，而是接收一个注册闭包；闭包拿到 `notify`：

```rust
let promise = Promise::create_from_future(ctx, |notify| {
    // 在 JS 线程或已有的轮询逻辑里拿到结果后调用；`notify` 没有 Send bound，
    // 不要把它 move 到其他线程。
    notify(Ok(JSValue::from(42)));
})?;
```

`notify(Ok(v))` fulfill，`notify(Err(reason))` reject。结果在后续 tick 中
settle，仍在 Zuqe 的 job + tick 系统内。跨线程结果请用
`create_with_channel` / `create_with_result_slot` 传入，再在 `convert` 里
构造 `JSValue`。

## 5. 与事件循环一起驱动

- 默认 `async_support`：`ctx.eval_wait(...).await` 已处理顶层 Promise；
  低层用 `tokio::join!(rt.run_async(), promise_future)`。
- 无 `async_support`：`eval_wait` 同步阻塞到循环空闲。
- `Runtime::run_async()` 返回 `RunFuture`，`Output = ExitStatus`；每次 poll
  推进 tick，直到没有工作。
- 不要在 `eval_wait` 里再手动传 `&mut Runtime`——Context 自己就属于一个
  Runtime，循环从 `ctx.rt()` 取。

## 6. JS 侧 Promise

标准 `Promise` 构造器与静态方法均可用：`Promise.resolve/reject/all/allSettled/
any/race/try/withResolvers`，实例 `then/catch/finally`。宿主通常只需返回
Rust 创建的 Promise，JS 侧照常 `await`。

## 7. 选择建议

| 场景 | API |
|---|---|
| 在 JS 线程轮询宿主状态 | `create_with_job` |
| 后台线程/异步运行时结果，单值 | `create_with_channel` |
| 已有 `WakeReceiver` | `create_with_receiver` |
| 单次写入槽，多 producer | `create_with_result_slot` |
| 已有回调式 Future 集成 | `create_from_future` |
| 未观察的 Promise 不应保活进程 | `*_hold_when_awaited` |

## 8. GC 注意

Promise、resolving functions、`convert` 闭包捕获的 JS 值都必须可达：

- 长寿命 Rust 结构里的 `Promise`/`JSValue` 用 `#[derive(GcTrace)]` 追踪。
- 从 traced 容器取出后跨 JS 调用，按 [gc-safety.md](gc-safety.md) Rule 4 保活。
- 修改 payload 中的 Promise/回调字段，按 Rule 5 走 guard slot + setter。
