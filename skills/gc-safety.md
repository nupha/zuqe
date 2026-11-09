# GC安全守则

> **目的：** 写 native function / module / class 时，只要 Rust 侧会持有或写入 **GC 节点**，
> 就必须保证它在 GC 下安全。本文件给出要遵守的 **规则（Rule 0-5）**、缺失后果、
> **最佳实践模板**与提交前 checklist。
>
> **本文件是 GC 规则的唯一来源。** trace 的实现示例/反例见 [`gc-trace.md`](gc-trace.md#2-实现示例)，
> 写屏障的实现模式与检查工具见 [`gc-write-barrier.md`](gc-write-barrier.md#3-检查流程)；
> 本文件不重复这些细节，只保留规则和链接。

## 1. GC 安全包含两部分

GC 正确性由两套机制共同保证，**缺一不可**：

| 部分 | 回答的问题 | 覆盖 | 细节 |
|---|---|---|---|
| **GC node trace** | 这个值还能被**全量标记**找到吗？ | full mark / major | [`gc-trace.md`](gc-trace.md#2-实现示例) |
| **node write barrier** | old 容器里新写入的 young 子对象，**minor** 能看到吗？ | minor / remembered set（`gc_generations`） | [`gc-write-barrier.md`](gc-write-barrier.md#2-实现模式) |

### 1.1 缺失 trace 会导致什么

GC 只从它知道的 roots 出发标记。Rust 侧的一个 `JSValue`/`Object` 放在 payload 字段、
`thread_local!` 或未被 trace 的 Rust 容器里时，对 collector 都不可见。

- 如果它是该对象唯一的引用 → full mark/major 把它当作垃圾回收；
- Rust 侧继续使用 → **use-after-free**；
- debug 常见 `gc: bad node`；release 可能随机崩溃、`TypeError`、回调错乱、teardown 崩溃；
- 短脚本可能一次 full mark 都没跑到，“能跑通”不代表 trace 正确。

### 1.2 缺失 write barrier 会导致什么

在 `gc_generations` 下，minor 只扫 young + remembered set，**不 descend old payload**。

- 一个已经变 old 的 owner 写入 young 子对象，却没有记录这条边；
- 下一次 minor 看不到它 → 回收 young 子对象；
- owner 仍保存引用 → **悬空引用 / UAF**；
- debug 在 minor 后 panic：`— missing write barrier`；release 静默 UAF；
- 常见表现：promise/异步操作永远不 settle、随机 `TypeError`、`gc: bad node`。

### 1.3 两者不能互相替代

- 只 trace 不 barrier：full mark 能看到，但 minor 丢子对象；
- 只 barrier 不 trace：minor 能看到，但 full mark 丢子对象；
- 增量/并发标记下同样需要 barrier：标记进行中的每条新边都必须让 collector 看见。

## 2. GC Rules（唯一规则来源）

> 具体的 API 用法、代码示例、反例和检查工具在 [`gc-trace.md`](gc-trace.md#2-实现示例) /
> [`gc-write-barrier.md`](gc-write-barrier.md#2-实现模式)；下面只给规则本身。

### Rule 0 — 黄金规则：Rust 侧持有的节点必须可达

任何被 Rust 侧持有的 GC 节点，都必须进入 GC 可达图。payload 字段、`thread_local!`、未被 trace
的 Rust 容器里的裸持有，都不在 collector 的视野里。

- 漏 trace 是致命的；不确定就 trace（对非节点 trace 是无害 no-op）。
- 这条规则覆盖宿主 payload、`thread_local!`、模块记录和跨 JS 调用中的临时持有。

细节：[`gc-trace.md`](gc-trace.md#1-不做-trace-的失败模式)。

### Rule 1 — payload 持有 GC 节点必须实现 `GcTrace`

- payload/长寿命 Rust 结构含 GC 节点 → 实现 `GcTrace`，**无条件 trace 每个 GC 字段**。
- 绝大多数情况用 `#[derive(GcTrace)]`（只依赖 zuqe 时加 `#[gc(crate_path = zuqe::gc_lite)]`）；只有
  derive 无法表达的特殊 trace 逻辑才手写 `impl GcTrace`，并在注释说明原因。
- 无 GC 节点时不要默认手写空 `impl GcTrace`：零尺寸 payload 用 `()` 无 payload 形式；非零尺寸且字段
  不实现 `GcTrace` 用 `unsafe_empty_gc_trace!`；字段都可 trace 时仍用 derive。见
  [`gc-trace.md` §2.6](gc-trace.md#26-无-gc-节点的-payload)。
- `extends:` 子类不共享父类 payload/trace。
- lazy cache（`RefCell<Option<Object>>`、`Vec<JSValue>`、缓存 map 等）只从 payload 可达时也要 trace。
- weak/不追踪字段用 `#[gc(skip)]`；不要 trace weak target。
- **`GcTrace` 不是 write barrier**；后写路径还要按 Rule 5 处理。

细节与示例：[`gc-trace.md`](gc-trace.md#21-payload-的-gctrace对应-rule-1)。

### Rule 2 — `thread_local!` 持有 GC 节点：注册 user trace

- 每个持有 GC 节点的 `thread_local!` 写一个 trace fn，覆盖所有 GC 字段。
- 在每个相关 runtime/thread 上用 `gc_set_trace_handler` 注册。
- 注册的 user trace 是 root；该 root 豁免不扩展到普通 old 容器或 payload。

细节与示例：[`gc-trace.md`](gc-trace.md#22-thread_local-的-trace对应-rule-2)。

### Rule 3 — `Vec` 存储中的裸指针

- 不要缓存可增长 `Vec` 的元素指针；每次变更后从索引重新取。
- 确需元素地址稳定时，用 `Vec<Box<T>>`。

细节与反例：[`gc-trace.md`](gc-trace.md#25-vec-裸指针对应-rule-3)。

### Rule 4 — 值离开 traced 容器后跨 JS re-entry

- 从 traced 容器 `remove`/`pop`/`take` 后，若还要调用可能运行 JS 的 API，必须先把值放进
  对象拥有的 traced slot，并覆盖所有退出路径（含 `?`）。
- scope 保护不能跨 safepoint 保活；**禁止**用 `gc_protect_transient` 作为修复。
- `RefCell` borrow 不要跨 JS；不要用“全局 roots `Vec` + 手动 push/pop”。
- root scope 的值活到 Context 销毁；临时工作用 `with_new_scope`，不再需要的值 `remove_local`。

细节、示例与反例：[`gc-trace.md`](gc-trace.md#23-跨-js-调用前保活对应-rule-4)。

### Rule 5 — old 容器接收 young 值（写屏障）

- `gc_generations` 下，old 节点接收 young 值必须记录 old→young 边；假设任何可变容器都可能是 old。
- 首选 **字段私有 + setter + guard slot**（`WriteBarrier`/`WriteBarrierVec`/`WriteBarrierMap`）：
  `set`/`push`/`insert` 一次完成 store + bind。
- owner 必须是持有 payload 的节点；store 与 bind 之间不得有 GC point。
- 容器元素必须实现 `GcBind`；weak/`#[gc(skip)]` 跳过。
- 禁止 whole-field replacement（`field = WriteBarrier::new(...)`、`mem::replace`）。
- 移除本身不需要 barrier，但移除后的值失去 trace 覆盖；跨 GC point 继续使用按 Rule 4 处理。
- 引擎已覆盖的写入（property slot/array/Map/Set/closure VarRef/`replace_data`/JIT/root containers）
  不要手写重复 barrier；**payload 字段不在其列**。
- 增量/并发标记下要求更严：标记进行中的每条新边都必须记录。

细节、API、示例与工具：[`gc-write-barrier.md`](gc-write-barrier.md#2-实现模式)。

> GC 的检查流程（静态审计、动态 minor、gc stress）与工具见 [`gc-write-barrier.md`](gc-write-barrier.md#3-检查流程)；
> 嵌入开发的全局流程与 checklist 见 [`workflow-checklist.md`](workflow-checklist.md#标准流程)。

## 3. 最佳实践模板

> 从样板开始：先复制下面的结构，再按实际类型/API 调整。细节规则见 §2；trace/屏障的更多写法见
> [`gc-trace.md`](gc-trace.md#2-实现示例) / [`gc-write-barrier.md`](gc-write-barrier.md#2-实现模式)。

```text
my-crate/
├── Cargo.toml
├── gc_audit_exemption.txt      # 唯一 exemption 来源；无条目可暂不建
├── src/
│   └── foo.rs                  # payload、GcTrace、guard slots、setter
└── tests/
    └── gc_audit.rs             # zuqe::gc_audit_test!(<root floor>);
```

`src/foo.rs`：

```rust
use std::ptr::NonNull;

use zuqe::{
    GcBind, JSValue, Object, ScopedContext, WriteBarrier, WriteBarrierMap, WriteBarrierVec,
    def_native_object,
    gc_lite::{GcHead, GcTrace},
};

/// 容器元素类型：局部 struct（tuple 不能 impl 外部 trait GcBind）。
#[derive(Debug)]
struct Req {
    once: bool,
    cb: Object,
}

impl GcBind for Req {
    fn bind_node(&self, ctx: &ScopedContext, owner: NonNull<GcHead>) {
        GcBind::bind_node(&self.cb, ctx, owner);
        // weak 引用跳过；与 #[gc(skip)]/GcTrace 行为对齐
    }
}

#[derive(Debug, GcTrace)]
#[gc(crate_path = zuqe::gc_lite)]
pub(crate) struct FooPayload {
    // full mark：derive 按字段展开 trace
    result: WriteBarrier<JSValue>,
    handler: WriteBarrier<Option<Object>>,
    // minor：guard slot 负责 store + bind；元素类型 impl GcBind
    queue: WriteBarrierVec<Req>,
    cache: WriteBarrierMap<u64, Object>,
}

// GcTrace 由 derive 生成，新增字段不会漏；只有特殊 trace 逻辑才手写 impl。
// 若 FooPayload 无 GC 节点且字段不实现 GcTrace，用 unsafe_empty_gc_trace!(FooPayload)；
// 若它根本没有状态，直接使用 () 无 payload 形式。不要写空 impl GcTrace。

impl FooPayload {
    // 只暴露 setter；owner 由调用点传入（实例自身）
    pub(crate) fn set_result(&mut self, ctx: &ScopedContext, owner: JSValue, v: JSValue) {
        self.result.set(ctx, owner, v);
    }

    pub(crate) fn set_handler(&mut self, ctx: &ScopedContext, owner: JSValue, v: Option<Object>) {
        self.handler.set(ctx, owner, v);
    }

    pub(crate) fn push_req(&mut self, ctx: &ScopedContext, owner: JSValue, r: Req) {
        self.queue.push(ctx, owner, r);
    }

    pub(crate) fn cache_put(&mut self, ctx: &ScopedContext, owner: JSValue, k: u64, v: Object) {
        self.cache.insert(ctx, owner, k, v);
    }
}

def_native_object!(
    "Foo",
    JSFoo,
    FooPayload,
    FOO_PROTO, // proto/ctor 见 native-class.md
    Some(ClassCtorDef::new(js_foo_constructor, 1, false)),
);
```

调用点：

```rust
let owner = this;                       // 在借用 payload 前捕获 owner
let p = get_foo_payload_mut(ctx, &mut this)?;
p.set_handler(ctx, owner, Some(cb))?;   // setter 内 store + bind
```

`tests/gc_audit.rs`：

```rust
//! Static GC audit: bare payload fields, missing GcBind impls,
//! whole-field guard replacement, stale exemptions.
zuqe::gc_audit_test!(10);
```

落地顺序：

1. 按 Rule 0-5 设计字段：GC 字段 → trace；可能后写 → guard slot/setter。
2. 实现 `GcTrace` 与 setter；容器元素实现 `GcBind`。
3. 建 `tests/gc_audit.rs` 与 `gc_audit_exemption.txt`，跑 `cargo test --test gc_audit` 红→绿。
4. 在 debug + `gc_generations` 下做定向 old→young 验证，必要时跑 `--gc-stress`。
5. 过一遍全局 [`workflow-checklist.md`](workflow-checklist.md#重点检测-checklist)。

## 4. 提交前 Checklist（引用）

> 按 [`gc-safety.md`](gc-safety.md) Rule 0-5 逐条检查；这里的条目只指向对应文档，不重复规则。

- [ ] Rule 0-2：每个 GC 字段都有可达性方案（payload/thread-local/跨 JS）。实现示例见 [`gc-trace.md`](gc-trace.md#21-payload-的-gctrace对应-rule-1)。
- [ ] 无 GC 节点的 payload 选对形态：零尺寸用 `()`，非零尺寸且字段不实现 `GcTrace` 用
  `unsafe_empty_gc_trace!`；没有用空手写 impl 掩盖字段。审计会检查宏类型不含 GC 节点，见
  [`gc-trace.md` §2.6](gc-trace.md#26-无-gc-节点的-payload)。
- [ ] Rule 4：跨 JS 的值有 traced slot 保活；无 `gc_protect_transient`/scope 依赖；`RefCell` 不跨 JS。见 [`gc-trace.md`](gc-trace.md#23-跨-js-调用前保活对应-rule-4)。
- [ ] Rule 5：后写字段有 guard slot/setter；owner 正确；无 whole-field replacement；元素有 `GcBind`。见 [`gc-write-barrier.md`](gc-write-barrier.md#2-实现模式)。
- [ ] 静态审计：`cargo test --test gc_audit` 通过，exemption 有理由且无 stale。见 [`gc-write-barrier.md`](gc-write-barrier.md#4-静态审计zuqe-gc-auditshape)。
- [ ] 动态验证：定向 old→young 路径过 minor；定向用例跑过 `--gc-stress`。见 [`gc-write-barrier.md`](gc-write-barrier.md#5-动态-minor-验证timing)。
- [ ] 评审：过一遍 [`workflow-checklist.md`](workflow-checklist.md#重点检测-checklist)。
