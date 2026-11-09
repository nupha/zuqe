# GC 追踪：实现示例与反例

> **规则唯一来源：** [`gc-safety.md`](gc-safety.md) §2 的 Rule 0-5。本文件**不重复规则**，只给
> trace 的实现示例、反例和排查方法；写屏障与检查工具见 [`gc-write-barrier.md`](gc-write-barrier.md#3-检查流程)。

> **绝大多数情况用 `#[derive(GcTrace)]`。** 持有 GC 节点的类型（payload、元素类型、长寿命状态
> 结构）都用 derive，让每个字段和枚举变体自动 trace。**只有确实存在自定义 trace 逻辑时**
> （条件 trace、特殊容器、特殊 weak 语义等）才手写 `impl GcTrace`，并在代码里注释说明原因。
> 没有特殊原因，不要手写；手写 impl 漏字段不会被静态审计发现。
>
> **不持有 GC 节点的类型**也不要手写空 impl：零尺寸用 `()` 无 payload；非零尺寸且字段不实现
> `GcTrace` 用 `unsafe_empty_gc_trace!`（见 §2.6）。

## 1. 不做 trace 的失败模式

GC 只从它知道的 roots 出发标记。Rust 侧的 `JSValue`/`Object` 放在 payload 字段、
`thread_local!` 或未被 trace 的 Rust 容器里时，对 collector 都不可见；如果它是唯一引用，下一次
full mark/major 会把它回收，Rust 侧继续使用就是 UAF。

- debug 常见：`gc: bad node`。
- release 可能：随机崩溃、`TypeError`、回调错乱、teardown 崩溃。
- 短脚本可能一次 full mark/major 都没跑到；不能因为“能跑通”就认为 trace 正确。

trace 管 full mark，barrier 管 minor；两者分工见 [`gc-safety.md`](gc-safety.md) §1。

## 2. 实现示例

### 2.1 payload 的 `GcTrace`（对应 Rule 1）

**绝大多数情况用 derive。** 只要类型持有 GC 节点，先写：

```rust
use zuqe::gc_lite::GcTrace;

#[derive(GcTrace)]
#[gc(crate_path = zuqe::gc_lite)]
struct MyPayload {
    cb: Object,
    value: Option<Object>,
    items: Vec<JSValue>,
}
```

- `#[gc(crate_path = zuqe::gc_lite)]` 用于**只依赖 `zuqe`** 的 crate；直接依赖 `gc-lite` 时可省略
  （默认展开为 `::gc_lite::…`）。
- derive 会按字段、按 enum 变体展开；以后新增字段/变体自动进入 trace，不会漏。
- 空/无 GC 节点的 payload 不要一律 derive：零尺寸用 `()` 无 payload；非零尺寸且字段不实现
  `GcTrace` 用 `unsafe_empty_gc_trace!`。见 [§2.6](#26-无-gc-节点的-payload)。

字段级属性只影响单个字段，其余字段仍由 derive 自动 trace。

**`#[gc(skip)]`：不追踪该字段。** 用于 weak target 或不由 GC 管理的句柄：

```rust
use zuqe::gc_lite::{GcTrace, GcWeak};

#[derive(GcTrace)]
#[gc(crate_path = zuqe::gc_lite)]
struct MyPayload {
    cb: Object,
    // weak 引用不保持目标存活，不能 trace；没有 skip 时 derive 也会因
    // Option<GcWeak<Object>> 不实现 GcTrace 而编译失败。
    #[gc(skip)]
    weak_target: Option<GcWeak<Object>>,
}
```

**`#[gc(with = "path")]`：用指定函数 trace 这个字段。** 当字段类型本身没有 `GcTrace`，或需要特殊
遍历时使用：

```rust
use zuqe::gc_lite::{GcTrace, GcTraceCtx};

struct MyHandle {
    nodes: Vec<JSValue>,
}

// derive 生成的调用形式是 path(&self.handle, gcx)。
fn trace_handle(handle: &MyHandle, gcx: &mut GcTraceCtx) {
    for node in &handle.nodes {
        node.trace(gcx);
    }
}

#[derive(GcTrace)]
#[gc(crate_path = zuqe::gc_lite)]
struct MyPayload {
    cb: Object,
    #[gc(with = "trace_handle")]
    handle: MyHandle,
}
```

`skip` 不能用在真正的 strong GC 边上；`with` 必须覆盖该字段的所有 GC 子节点，否则和手写 trace 漏
字段一样危险。

**只有 derive 无法表达时才手写 `impl GcTrace`**，例如条件 trace、第三方容器、特殊 weak 语义等：

```rust
use zuqe::gc_lite::{GcTrace, GcTraceCtx};

impl GcTrace for MyPayload {
    fn trace(&self, gcx: &mut GcTraceCtx) {
        // 特殊逻辑；逐字段核对
        self.cb.trace(gcx);
        // ...
    }
}
```

手写 impl 的责任更大：新增字段时必须同步修改；**静态审计不解析 trace 方法体**，漏字段不会被它发现。
因此除非有明确理由，不要手写。

### 2.2 `thread_local!` 的 trace（对应 Rule 2）

规则见 [`gc-safety.md` Rule 2](gc-safety.md#rule-2--thread_local-持有-gc-节点注册-user-trace)。

```rust
thread_local! {
    static MY_STATE: RefCell<HashMap<u64, JSValue>> = const { RefCell::new(HashMap::new()) };
}

fn gc_trace_my_state(gcx: &mut GcTraceCtx) {
    MY_STATE.with_borrow(|map| {
        for v in map.values() { v.trace(gcx); }
    });
}

rt.gc_set_trace_handler(|gcx| {
    gc_trace_my_state(gcx);
    // 每个持有 GC 节点的 thread-local 一行
});
```

### 2.3 跨 JS 调用前保活（对应 Rule 4）

规则见 [`gc-safety.md` Rule 4](gc-safety.md#rule-4--值离开-traced-容器后跨-js-re-entry)。

```rust
// BAD：值离开 traced 容器后，JS 调用可能回收它
let chunk = queue.borrow_mut().remove(0);
call_js_that_may_gc(ctx)?;
use(chunk); // 可能 UAF

// GOOD：放进对象拥有的 traced slot，closure 退出时释放
cp.with_in_flight(owner, |held| -> Result<Promise, i32> {
    held.push(ctx, chunk);              // traced until the closure returns, and bound
    if cp.should_call_pull(ctx)? { cp.call_pull(ctx)?; }
    iterator_result_promise(ctx, chunk, false)
});
```

若 hold 放在 payload slot，写入路径还必须按 Rule 5 barrier；见
[`gc-write-barrier.md`](gc-write-barrier.md#2-实现模式)。

### 2.4 scope / root（对应 Rule 4 的 scope 部分）

```rust
ctx.with_root_scope(|mut root| {
    root.with_new_scope(|scope| {
        let tmp = Array::create(scope)?; // 只在 child scope 内受保护
        Ok(tmp.into())                   // 返回值 promotion 到父 scope
    })
});
```

root scope 的值活到 Context 销毁；临时工作放 `with_new_scope`，不再需要的 root 值用
`remove_local`。

### 2.5 `Vec` 裸指针（对应 Rule 3）

规则见 [`gc-safety.md` Rule 3](gc-safety.md#rule-3--vec-存储中的裸指针)。

```rust
// BAD：缓存元素指针，随后 push 可能 realloc，指针悬垂
let p: *mut Item = &mut items[0];
items.push(Item::new()); // 可能 realloc
unsafe { (*p).touch(); } // 悬垂指针

// GOOD：每次从索引重新取；或让地址稳定
items.push(Item::new());
let p = &mut items[0];
```

需要稳定地址时用 `Vec<Box<T>>`。

### 2.6 无 GC 节点的 payload

payload 没有 GC 节点时，不要默认手写空 `impl GcTrace`。按存在形式选择最小方案：

1. **零尺寸/无状态**：用 `def_native_object!` 的 `()` 无 payload 形式，删掉 payload 类型。

   ```rust
   def_native_object!(
       "TextEncoder",
       JSTextEncoder,
       (),
       TEXT_ENCODER_PROTO,
       Some(ctor),
   );
   // JSTextEncoder::new_instance(ctx, new_target)
   ```

2. **非零尺寸，字段都实现 `GcTrace`**：仍用 `#[derive(GcTrace)]`。即使当前全是 no-op 字段，新增
   GC 字段时会自动进入 trace。

3. **非零尺寸，字段不实现 `GcTrace`（纯 Rust 句柄、第三方容器、tokio 同步原语等）**：用
   `zuqe::gc_lite::unsafe_empty_gc_trace!`：

   ```rust
   pub(crate) struct BlobPayload {
       bytes: Arc<[u8]>,
       type_: String,
   }
   // 明确断言：payload 传递闭包内没有 GC 节点。
   zuqe::gc_lite::unsafe_empty_gc_trace!(BlobPayload);
   ```

4. **任何字段（包括 `#[gc(skip)]` 字段）可能持有 GC 节点**：不适用空 trace；必须 trace，或按
   weak/外部句柄语义明确处理。

`unsafe_empty_gc_trace!` 只展开空 `trace`，不改变 payload 生命周期；`payload_drop` 仍由
`def_native_object!` 正常接线。`unsafe_` 前缀是契约标记：类型传递闭包必须没有 GC 节点，新增 GC 字段
后必须删除宏并补 trace。`gc_audit` 会递归检查这些类型，包在 `WriteBarrier` 里的 GC 字段也会被抓到。

## 3. 反例集

### 3.1 漏 trace 一个字段

违反 [`gc-safety.md` Rule 1](gc-safety.md#rule-1--payload-持有-gc-节点必须实现-gctrace)。

```rust
struct MyPayload {
    cb: Object,
    value: Option<Object>,
}

impl GcTrace for MyPayload {
    fn trace(&self, gcx: &mut GcTraceCtx) {
        self.cb.trace(gcx); // 漏了 self.value
    }
}
```

`value` 只从 payload 可达时会被 full mark 回收，后续使用 UAF。

如果这个类型用 `#[derive(GcTrace)]`，新增 `value` 字段会自动进入 trace，这个错误在类型层面就不会发生。

### 3.2 `extends:` 子类复用父类 payload/trace

违反 [`gc-safety.md` Rule 1](gc-safety.md#rule-1--payload-持有-gc-节点必须实现-gctrace)。

```rust
struct ParentPayload { p: Object }
struct ChildPayload { c: Object } // 没实现自己的 GcTrace
```

GC 按对象自己的 `class_id` 查找 trace；子类字段不会被父类 trace 覆盖。每个子类 payload 都要各自
`#[derive(GcTrace)]`（或手写自己的 impl）。

### 3.3 懒缓存 JS 对象未 trace

违反 [`gc-safety.md` Rule 1](gc-safety.md#rule-1--payload-持有-gc-节点必须实现-gctrace)。

```rust
struct UrlPayload {
    search_params: RefCell<Option<Object>>, // 只从 payload 可达，却没有 trace
}
```

`searchParams`、解析结果、namespace、`import.meta` 等 lazy cache 都可能踩这个坑。

### 3.4 `thread_local!` 存了节点但没注册 trace

违反 [`gc-safety.md` Rule 2](gc-safety.md#rule-2--thread_local-持有-gc-节点注册-user-trace)。

```rust
thread_local! {
    static CACHE: RefCell<Vec<Object>> = const { RefCell::new(Vec::new()) };
}
// 没有 gc_set_trace_handler，也没有覆盖 CACHE 的 trace fn
```

节点对 collector 不可见，随时可能被回收。

### 3.5 `remove`/`pop`/`take` 后跨 JS 使用

违反 [`gc-safety.md` Rule 4](gc-safety.md#rule-4--值离开-traced-容器后跨-js-re-entry)。

```rust
let cb = list.borrow_mut().pop().unwrap();
cb.call(ctx, JSValue::UNDEFINED, &[])?; // 可能 GC；cb 已离开 traced 容器
```

先把 `cb` 放回 traced slot（closure-scoped），再调用 JS。

### 3.6 `RefCell` borrow 跨 JS

违反 [`gc-safety.md` Rule 4](gc-safety.md#rule-4--值离开-traced-容器后跨-js-re-entry)。

```rust
let mut list = cbs.borrow_mut();
while let Some(cb) = list.pop() {
    cb.call(ctx, JSValue::UNDEFINED, &[])?; // 持有 borrow_mut 跨 JS
}
```

GC 在 JS 调用中 trace 同一个 cell 时会 `borrow()`，与未释放的 `borrow_mut()` 冲突，panic
`already borrowed`。每次取出一个值后立即释放借用，再调用 JS。

### 3.7 依赖 `gc_protect_transient`

违反 [`gc-safety.md` Rule 4](gc-safety.md#rule-4--值离开-traced-容器后跨-js-re-entry)。

```rust
gc_protect_transient(value); // feature 未开启时 no-op，且仍依赖 scope
call_js(ctx)?;
use(value);
```

正确做法是 Rule 4 的 traced slot。

### 3.8 在 root scope 堆积临时值

违反 [`gc-safety.md` Rule 4](gc-safety.md#rule-4--值离开-traced-容器后跨-js-re-entry) 的
scope/root 部分。

```rust
ctx.with_root_scope(|mut root| {
    let tmp = root.create_base_object()?;
    // 用完不 remove_local，泄漏到 Context 销毁
    Ok(())
});
```

临时工作用 `with_new_scope`；确实放进 root scope 的值，不再需要时 `remove_local`。

### 3.9 只 trace 不 barrier

违反 [`gc-safety.md` Rule 5](gc-safety.md#rule-5--old-容器接收-young-值写屏障)。

```rust
// trace 覆盖了 slot，但实例可能已 old 后直接赋值
self.cache = Some(obj);
```

full mark 能看到，但 minor 不 descend old payload，`obj` 仍可能被回收。写入路径见
[`gc-write-barrier.md`](gc-write-barrier.md#2-实现模式)。

### 3.10 trace weak target

违反 [`gc-safety.md` Rule 1](gc-safety.md#rule-1--payload-持有-gc-节点必须实现-gctrace) 的
weak/`#[gc(skip)]` 部分。

```rust
impl GcTrace for Payload {
    fn trace(&self, gcx: &mut GcTraceCtx) {
        self.weak_target.trace(gcx); // weak 不应被 trace，否则目标永远不会死
    }
}
```

`GcBind` 中也要保持同样的跳过语义。用 derive 时给 weak 字段标 `#[gc(skip)]`；不要手写 trace
weak target。

### 3.11 用 `unsafe_empty_gc_trace!` 掩盖 GC 字段

违反 [`gc-safety.md` Rule 1](gc-safety.md#rule-1--payload-持有-gc-节点必须实现-gctrace)。

```rust
struct MyPayload {
    cb: Object,
}

// 错误：Object 不会被 trace
zuqe::gc_lite::unsafe_empty_gc_trace!(MyPayload);
```

空 trace 类型在 full mark 时完全不会被访问；即使字段包在 `WriteBarrier` 里也不行，因为没有任何代码
会走到它。`gc_audit` 的 `empty_trace_violations` 会递归检查被宏标记的类型，发现可达 GC 节点即失败。
宏只能用于确实没有任何 GC 节点的类型；不确定时回到 derive 或 `#[gc(skip)]`。

## 4. 排查 trace 问题

1. **先看是不是 derive**：payload 绝大多数情况应使用 `#[derive(GcTrace)]`；只有手写 `impl GcTrace` 时，
   才需要逐字段对照 payload 里每个 `JSValue`/`Object`/wrapper/回调句柄是否都出现在 `trace`。
2. **生命周期**：这个值是否只从 payload/thread-local 可达？是 → 检查对应 Rule 1/2。
3. **`extends:`**：子类是否有自己的 payload 和 `GcTrace`？
4. **lazy cache**：`RefCell<Option<Object>>`、`Vec<JSValue>`、map 是否被覆盖？
5. **weak**：weak target 是否被错误 trace 或 bind？
6. **跨 JS**：从 traced 容器取出的值是否在 JS 调用后继续使用？`RefCell` borrow 是否跨 JS？
7. **无 GC 节点判定**：如果用了 `unsafe_empty_gc_trace!`，确认类型传递闭包没有 GC 节点；
   `gc_audit` 的 `empty_trace_violations` 会报 `unsafe_empty_gc_trace! type contains GC node`。
8. **静态审计交叉检查**：`cargo test --test gc_audit` 会报告裸 GC 字段（shape）；它不证明 trace
   写对，但能提示漏掉的可追踪字段。工具见 [`gc-write-barrier.md`](gc-write-barrier.md#4-静态审计zuqe-gc-auditshape)。
9. **debug full/major**：用宿主提供的 GC API（如 zjs 的 `std.gc()`）制造回收，观察是否出现
   `gc: bad node`；长时间/压力运行也能暴露。

trace 规则本身请直接对照 [`gc-safety.md`](gc-safety.md) Rule 1/2/4；本文件不再重复。
