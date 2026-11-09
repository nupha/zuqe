# 写屏障：实现模式与工具

> **规则唯一来源：** [`gc-safety.md`](gc-safety.md) §2 Rule 0-5（重点是 Rule 5；Rule 1/4 是前提）。
> 本文件**不重复规则**，只给写屏障的实现模式、失败模式、静态审计/动态 minor/gc stress 工具。
> trace 实现与反例见 [`gc-trace.md`](gc-trace.md#2-实现示例)。

## 1. 不做 write barrier 的失败模式

在 `gc_generations` 下：

- 新分配是 **young**；minor 只扫 young + **remembered set**，**不 descend old payload**；
- 一个已经变 old 的 owner 直接写入 young 子对象，却没有记录这条边；
- 下一次 minor 看不到这条边，会把 young 子对象当作垃圾回收；
- owner 仍然保存着它的引用 → **悬空引用 / use-after-free**。

debug 构建会在 minor 后立即检查（`dbg_assert_no_stale_old_edges`），典型 panic：

```text
a live object still points at a node freed by the last minor:
child 0x20000c7aa80 (freed dtype Some(1)), owner 0x20007001e00 (dtype Some(1))
— missing write barrier
```

release 构建没有这个 validator，同一漏屏障不会 panic，而是**静默的悬空引用 / UAF**。

为什么“脚本能跑通”不代表安全：

- minor 只在 young 分配超过阈值或显式请求时发生；短脚本可能一次 minor 都没跑到。
- owner 可能一开始是 young，跑一段时间后才被 promote 成 old；问题只在 promote 之后写入才出现。
- release 没有 debug validator；很多漏屏障只在压力/长跑/真实负载下才表现为远距离崩溃。
- 同一漏屏障在 debug 可能表现为 `missing write barrier`，在 release 可能表现为
  `gc: bad node`、随机 `TypeError`、teardown 崩溃、promise 永远不 settle。

若引擎/宿主启用**步进式或并发标记**，要求更严：标记进行中 mutator 新建/改写的每条边都必须让
collector 看见，否则会“黑色对象指向白色对象”漏标。屏障仍统一走 guard slot → `bind`。

## 2. 实现模式

> 规则、owner 约束、store/bind 间隔、`GcBind` 要求等见
> [`gc-safety.md` Rule 5](gc-safety.md#rule-5--old-容器接收-young-值写屏障)。

### 2.1 guard slots

```rust
#[derive(Debug)]
struct FooPayload {
    result: WriteBarrier<JSValue>,          // p.result = v 不能编译
    queue: WriteBarrierVec<Box<Req>>,       // queue.push(v) 不能编译
    cache: WriteBarrierMap<u64, Object>,    // cache.insert(k, v) 不能编译（裸 Map）
}

impl FooPayload {
    fn set_result(&mut self, ctx: &ScopedContext, owner: JSValue, v: JSValue) {
        self.result.set(ctx, owner, v); // store + barrier，一步完成
    }
}
```

guard slot 只负责屏障；payload 的 `GcTrace` 仍要单独实现（见 [`gc-trace.md`](gc-trace.md#21-payload-的-gctrace对应-rule-1)）。
构造期与实例存在后的写入方式见 [`gc-safety.md` Rule 5](gc-safety.md#rule-5--old-容器接收-young-值写屏障)。

### 2.2 setter + owner

```rust
// GOOD：同一 setter 完成存储并 bind 每条边。
pub(crate) fn set_capability(
    &mut self,
    ctx: &ScopedContext,
    promise: Promise,
    funcs: [JSValue; 2],
) {
    let owner = self.gc_head_ptr();
    self.promise.set_node(ctx, owner, Some(promise));
    self.resolving_funcs[0].set_node(ctx, owner, funcs[0]);
    self.resolving_funcs[1].set_node(ctx, owner, funcs[1]);
}
```

调用点通常在拿到 payload 可变借用前捕获 owner：

```rust
let owner = this;                       // JSValue 是 Copy
let p = get_foo_payload_mut(ctx, &mut this)?;
p.set_handler(ctx, owner, Some(cb))?;   // setter 内 store + bind
```

> 更安全的做法：把 setter 放到 native wrapper 上，内部 `let owner = JSValue::from(*self);`，
> 调用方不传 owner（见 §5.3）。static audit 无法验证 owner，只有这种 API 设计能消除传错的机会。

### 2.3 `GcBind`（容器元素）

```rust
impl zuqe::GcBind for Req {
    fn bind_node(&self, ctx: &zuqe::ScopedContext, owner: std::ptr::NonNull<zuqe::gc_lite::GcHead>) {
        zuqe::GcBind::bind_node(&self.cb, ctx, owner);
        // weak 引用跳过（与 #[gc(skip)]/GcTrace 行为对齐）
    }
}
```

实现细节（元素类型要求、tuple 限制、weak/`#[gc(skip)]` 跳过）见
[`gc-safety.md` Rule 5](gc-safety.md#rule-5--old-容器接收-young-值写屏障)。元素类型实现 `GcBind`
后，从容器 `push`/`insert` 进入的路径不再报；但若该元素类型同时是 root 或被直接引用，它自身的
字段仍按普通 payload 字段判定，必要时按 exemption 规范记录，不要重复包装。

### 2.4 function / module / class 的落点

| 场景 | 屏障实现点 |
|---|---|
| **function binding** | 函数 data 走 `CallContext::replace_data`（引擎已含 barrier）；宿主长期状态放 payload / `thread_local` / user state |
| **module** | capability/resolver/namespace/async parent 等模块记录是长寿命 old 容器；统一 setter |
| **class payload** | 可变字段 guard slot + setter；容器元素 `GcBind` |
| **zapi** | 无 barrier ABI；引擎对 old zapi 实例每次 minor 重扫 payload，`gc_trace` 仍需报告全部字段 |

## 3. 检查流程

写屏障相关的检查顺序：

| 步骤 | 做什么 | 工具/位置 |
|---|---|---|
| 1. 设计自检 | 按 [`gc-safety.md`](gc-safety.md#2-gc-rules唯一规则来源) Rule 0-5 判断每个 GC 字段的 trace/后写路径 | [`gc-safety.md` §2](gc-safety.md#2-gc-rules唯一规则来源) |
| 2. 静态审计 | 查形态：裸字段、缺 `GcBind`、整体替换 guard、enum 变体 key、声明位置、未知类型、过期豁免、no-trace 类型含 GC 节点 | [本文件 §4](#4-静态审计zuqe-gc-auditshape) |
| 3. 动态 minor | 查时序：old→young 是否真的漏 barrier | [本文件 §5](#5-动态-minor-验证timing) |
| 4. gc stress | 提高 minor 频率，扫更多路径 | [本文件 §6](#6-gc-stress) |
| 5. review | 查工具看不到的语义：`GcTrace` 方法体、写入时序、owner、继承、exemption、边界 | [`workflow-checklist.md`](workflow-checklist.md#重点检测-checklist) |

结论口径：静态审计绿 + 定向 old→young 路径过 minor + 定向用例过 `--gc-stress` 是较强信号，
但不是形式化证明；release 正确性最终依赖 `#[derive(GcTrace)]`、guard slot、setter 约定和 review。

## 4. 静态审计：`zuqe-gc-audit`（shape）

这是一个**提交前的形态检查**：扫描你的 payload、字段类型和写入代码，找出“GC 边没有被 guard /
绑定覆盖”或“明显绕过 barrier”的写法，并把问题定位到 `file:line`。它不运行 GC，也不证明写入时序；
它做的事情是**把容易漏的 GC 风险变成一条能直接修的报错**。

工具本体叫 `zuqe-gc-audit`，它以命令行工具形式提供同一套检查，本质和 `cargo test --test gc_audit` 一样；日常建议只用测试入口。

日常只有一种用法：在目标 crate 里运行：

```bash
cargo test --test gc_audit
```

不需要再记第二个命令。测试失败时的输出就是报告；通过时说明这批形态规则全部通过。

### 4.1 它检查什么

| 你会遇到的写法 | 典型代码 | 为什么危险 |
|---|---|---|
| 裸 GC 字段 | `value: JSValue`、`handler: Option<Object>` | 没有 guard slot：old→young 后写不会记录；手写 trace 也容易漏掉 |
| 容器元素没有绑定 | `queue: WriteBarrierVec<Req>`，但 `Req` 没有 `impl GcBind` | push 时 young 元素没有进入 remembered set，minor 可能回收它 |
| 整体替换 guard | `self.slot = WriteBarrier::new(v)`、`*controller = WriteBarrier::new(...)`、`mem::replace` | 绕过 `set`/`push`/`insert`，边没有记录 |
| enum 变体同名字段 | `Kind::Default.pending_read` 与 `Kind::Byte.pending_read` | 工具按变体区分 key，避免给一个变体写的豁免误放过另一个 |
| payload 声明位置不对 | payload 写在 `mod`/函数内部（有缩进） | 整个 payload 没进审计，等于没检查 |
| no-trace 类型含 GC 节点 | `unsafe_empty_gc_trace!(FooPayload)`，但 `FooPayload` 有 `Object` 或 `WriteBarrier<...>` 字段 | 整类不 trace，full mark 永远看不到这些边；包在 `WriteBarrier` 里也不行 |
| 跨 crate 未知类型 | `handle: ForeignHandle` | 工具无法判断它是不是 GC 节点，只能提示你确认 |
| 过期豁免 | 字段已改名、已包裹或删除，豁免还在 | 豁免表腐烂，后续会误导 review |

其中 “no-trace 类型含 GC 节点” 是整类检查（`empty_trace_violations`）：被 `unsafe_empty_gc_trace!`
标记的类型必须传递闭包内没有 GC 节点。它由标准测试入口（`gc_audit_test!` → `all_violations`）和 CLI
一起执行，**不能用豁免表消掉**。发现后只能删宏并补 trace，或确认该类型确实不该有 GC 字段。

除此之外，扫描结果异常少（例如目录扫错、payload 注册方式没被识别）也会直接失败，防止“空扫也全绿”。

### 4.2 标准布局（crate 内）

只需要两个文件：

| 路径 | 作用 |
|---|---|
| `tests/gc_audit.rs` | 一行测试入口 |
| `gc_audit_exemption.txt` | **唯一**豁免文件；与 `Cargo.toml`、`src` 同级 |

```rust
// tests/gc_audit.rs
zuqe::gc_audit_test!(30);
```

`30` 是扫描到的 payload 类型数量下限，用来防止扫错目录。第一次可以估一个值；如果失败信息说实际数量更少，
按实际值调低到略小即可。**不要**为了掩盖“某个 payload 没被识别”而故意调低它。

两条硬规则：

1. **payload 类型必须顶层声明**（行首不缩进）。写在 `mod` 或函数里会报
   `payload type not parsed`，必须移出来。
2. **只维护一份豁免文件**：测试和工具都读 `gc_audit_exemption.txt`，不要在测试里另写一份 inline 列表。

### 4.3 运行与读结果

```bash
cargo test --test gc_audit
```

失败时，每条报告都是一行 `file:line: Type.field: 类型 — 原因`：

```text
GC payload audit failed:
  src/foo.rs:12: FooPayload.value: `JSValue` — bare GC field, not wrapped and not allowlisted
  src/foo.rs:20: FooPayload.queue: `WriteBarrierVec<Req>` — container element type has no `impl GcBind` and is not allowlisted
  src/foo.rs:31: *controller = WriteBarrier::new(Some(c)); — whole-field guard replacement (use the payload setter / set / push)
  src/foo.rs:40: FooPayload.cb: `Object` — `unsafe_empty_gc_trace!` type contains GC node `Object`; remove the macro or trace the field
```

`empty_trace_violations` 的条目也在同一份报告里；CLI 遇到它同样以非零状态退出。

按“原因”就能决定怎么修，不需要查源码或额外工具。通过时的输出类似：

```text
test gc_audit_payload_fields ... ok
```

这就是“形态检查通过”。它不代表写入时序、trace 方法体或 owner 已经正确；那几项分别见 §5 和 review。

### 4.4 工作流：红 → 绿

1. 改完 GC 相关代码后跑 `cargo test --test gc_audit`。
2. 按报错关键词修：

   | 报错关键词 | 怎么修 |
   |---|---|
   | `bare GC field` | 改成 `WriteBarrier` / `WriteBarrierVec` / `WriteBarrierMap`，后写走 setter |
   | `container element type has no impl GcBind` | 给元素类型实现 `GcBind`；weak/`#[gc(skip)]` 字段跳过 |
   | `whole-field guard replacement` | 不要 `field = WriteBarrier::new(...)`；走 setter 的 `set`/`push`/`insert` |
   | `payload type not parsed` | 把 payload 移到顶层（column 0），不要靠调低数量下限或加豁免 |
   | `unsafe_empty_gc_trace! type contains GC node` | 删除宏，改回 `#[derive(GcTrace)]` / 手写 trace / 按字段 `#[gc(skip)]`；只有确实无 GC 节点才保留宏 |
   | `stale allowlist entries` | 删除或更新对应豁免；enum 字段用 `Type::Variant.field` |
   | `unknown type` | 先确认它是不是 GC 节点；是就按 GC 字段保守补 guard/trace，否则忽略这条非阻塞提示 |

3. **trace 漏字段不能靠这个工具发现**。payload 绝大多数情况用：

   ```rust
   use zuqe::gc_lite::GcTrace;

   #[derive(GcTrace)]
   #[gc(crate_path = zuqe::gc_lite)]
   struct FooPayload { /* ... */ }
   ```

   这样新增字段/变体时 trace 自动跟上；手写 `impl GcTrace` 漏字段不会让 audit 变红。

4. 只有确属合法结构例外才加豁免，并在文件里写清原因。**不要**用豁免掩盖“还没迁移”。
5. 重跑直到通过。绿色只代表 shape 通过；真正的 old→young 时序验证仍走 §5。

### 4.5 Exemption 规范

格式：每行一条，支持 `#` 注释和空行。

```text
# Type.field — reason
FooPayload.cache — construction only
FooPayload.handler — weak target; not traced
ReadableControllerKind::Byte.pending_read — bound by the byte-controller setter
CommandArgs::Func.0 — captured before new_instance
```

字段 key：

- 普通 struct：`Type.field`
- enum 变体：`Type::Variant.field`
- enum tuple variant：`Type::Variant.N`

enum 的 key 是**变体级**的：给 `Kind::Byte.pending_read` 写的豁免不会放过
`Kind::Default.pending_read`。旧式的 `Kind.pending_read` 会变成 stale。

合法类别（选一个写清楚）：

1. **construction only** — 构造后不再写，实例还是 young 时值已确定。
2. **root container** — minor 会强制下探，barrier 没有意义。
3. **bound elsewhere** — 唯一写路径已经 bind（setter，或元素 `GcBind` 在 push 时绑定）。
4. **structural non-goal** — 裸指针镜像、weak target、单 setter 的 `Cell` 槽等。

要求：

- 原因具体、可审阅；禁止“先这样”“暂未迁移”式占位。
- stale 条目会让测试失败，保持列表短且与代码同步。
- `payload type not parsed` 不能用豁免消掉：整个 payload 没进审计，必须把类型移到顶层。
- `unsafe_empty_gc_trace!` 类型含 GC 节点不能用豁免消掉：必须删宏并补 trace，或确认类型确实无 GC 字段。
- `unknown type` 不是 GC 风险结论，也不靠豁免消除；它只是提示你确认该类型是不是 GC 节点。

### 4.6 什么时候该跑

- 新增/修改 payload 字段后；
- 新增/删除 `unsafe_empty_gc_trace!` 标记，或给被标记类型新增字段后；
- 新增容器元素类型、或修改 `GcBind` 后；
- 修改 setter、后写路径、`WriteBarrier` 构造/替换后；
- 新增 enum 变体后；
- 提交前、CI 里。

把 `cargo test --test gc_audit` 当作 GC 相关改动的**最低门槛**；它与 §5 的定向 minor 验证、
`--gc-stress` 和 review 叠加，才是完整的 GC 安全检查。

## 5. 动态 minor 验证（timing）

### 5.1 前置条件

| 条件 | 说明 |
|---|---|
| **debug 构建** | validator 与 `gc_minor_now` 只在 debug 存在；release 没有运行时验证 |
| **`gc_generations`** | old/young 分代与 remembered set 的基础；zuqe 默认 feature 已包含 |
| **能自己触发 minor** | 纯 native 路径不会自动到 safepoint；测试里要显式调用 `gc_minor_now` |

release / feature-off：`gc_set_stress`、`gc_minor_now` 相关 API 不存在或为 no-op。不要为了
“消掉 panic”关闭 `gc_generations` 或把 `gc_set_promote_after(0)` 当修复。

### 5.2 关键 API

| API | 作用 | 可见性/条件 |
|---|---|---|
| `gc_set_promote_after(n)` | 设置 young 晋升阈值；debug 默认 1，release 默认 2；`0` 表示只做 major | public |
| `gc_minor_request()` | **请求**下一次 safepoint 执行 minor；纯 native 路径不会自己到 safepoint | public |
| `gc_minor_now()` | **立即**执行一次 minor（测试用） | public，`debug_assertions` |
| `gc_minor_count()` | 已执行 minor 次数，用于断言测试确实跑了 minor | public |

> `gc_minor_now` 是宿主测试 API（如 `Aike.gcMinor`）；release 宿主不能依赖它。
> 引擎内部测试里的 `gc_minor()` 是 crate 内部方法，外部 crate 用 `gc_minor_now()`。

### 5.3 定向验证一条 old→young 路径

1. `gc_set_promote_after(1)`，确保 minor 后会晋升；
2. 创建 owner（native 实例 + payload），调用 `gc_minor_now()` 把 owner 晋升为 old；
3. 创建 young 子对象，并用 `remove_local(...)` 把它从 scope 保护中移除，
   确保它只由 payload 字段引用；
4. 通过 setter / guard slot 写入 owner 的字段（推荐 native wrapper setter：owner 由 `self` 推导）；
5. 再调用 `gc_minor_now()`：
   - 有 barrier：minor 通过，子对象仍可达；
   - 缺 barrier：panic，消息包含
     `a live object still points at a node freed by the last minor ... — missing write barrier`；
6. 断言值仍可用（不是只看“没 panic”）。

> 一个 owner 对应一个槽位做验证。`bind` 会按 owner 去重 remembered set；同一 owner 上另一个
> 正确绑定的字段可能重新 trace 整个 payload，从而掩盖本字段缺失的 bind。

测试模板（外部 crate）：

```rust
use zuqe::{JSValue, Runtime, ScopedContext};

#[cfg(all(debug_assertions, feature = "gc_generations"))]
#[test]
fn late_write_is_barriered() -> Result<(), i32> {
    let mut rt = Runtime::create()?;
    let mut ctx = rt.create_context(|cx| {
        ScopedContext::ECMA_CONTEXT_SETUP(cx)?;
        Ok(())
    })?;

    ctx.with_root_scope(|mut cx| -> Result<(), i32> {
        cx.rt_mut().gc_set_promote_after(1);

        let mut owner = /* JSFoo::new_instance(&mut cx, ...)? */;

        cx.rt_mut().gc_minor_now(); // owner -> old

        let child = cx.create_base_object()?;
        assert!(cx.remove_local(child.gc_node_ptr()));

        // setter 在 native wrapper 上：owner = JSValue::from(owner)，调用方不传 owner
        owner.set_cached(&cx, Some(child));

        cx.rt_mut().gc_minor_now(); // missing barrier panics here

        assert!(owner.payload().expect("payload").cached().is_some());
        Ok(())
    })
}
```

要点：

- **owner 由 setter 推导**：`set_controller` / `set_stream` / `set_algorithms` / `set_byte_self_handle`
  这类 setter 放在 native wrapper 上，内部用 `JSValue::from(*self)`，调用方没有传错的机会。
- 若 wrapper/payload 的既有 API 仍要求调用方传 `owner`，必须在借 payload 前捕获正确的实例值；
  static audit 查不出 owner 传错，只有 validator + review 能兜底。
- 只有走到 `gc_minor_now()` 的路径才会被 validator 检查；纯 native 代码不经过 safepoint，
  必须自己触发。
- 不要在测试里用“再 bind 一次”来掩盖问题；按真实生产写路径走 setter。
- 若字段是容器，用 `WriteBarrierVec::push` / `WriteBarrierMap::insert`；元素类型要有 `GcBind`。

### 5.4 失败信息怎么读

```text
a live object still points at a node freed by the last minor:
child 0x20000c7aa80 (freed dtype Some(1)), owner 0x20007001e00 (dtype Some(1))
— missing write barrier
```

- **owner**：仍存活的 old 节点；
- **child**：上一次 minor 回收掉的 young 节点；
- 两者之间的字段边在写入时没有记录到 remembered set。

排查顺序：

1. 用 `cargo test --test gc_audit` 找裸字段或 whole-field replacement；
2. 检查该字段的所有后写路径是否都经过 setter/`set`/`push`/`insert`；
3. 检查 owner 是否传错（尤其共享结构、跨模块写入）；
4. 检查容器元素是否实现了 `GcBind`，weak/`#[gc(skip)]` 是否跳过；
5. 检查该值是否其实是从别的 traced 容器 `remove` 出来后直接写入（先按 `gc-safety.md` Rule 4
   检查保活）；
6. 只有确认是合法结构情况才加 exemption，不要在字段上“再 bind 一次”掩盖。

release 构建里同一漏 barrier 不会 panic，而是静默 UAF。不要把 debug panic 当成“仅测试噪音”。

## 6. gc stress

stress 模式让**每次 GC 节点分配**都请求 minor，从而让 validator 看到几乎每一条 old→young 写入。
适合在定向单测之外，用真实脚本/集成用例再扫一遍。

如果宿主提供了 `--gc-stress`（zuqe 的 `zjs`、aike 等），直接跑定向脚本/测试：

```bash
# zuqe 自带示例
./target/debug/examples/zjs --gc-stress tests/test_gc_barriers.mjs
./target/debug/examples/zjs --gc-stress tests/test_gc_stress_regress.mjs

# 宿主若有同类 flag（以宿主 CLI 为准）
./target/debug/aike --gc-stress path/to/targeted-test.mjs
```

也可在测试/宿主代码里开启：

```rust
#[cfg(all(debug_assertions, feature = "gc_generations"))]
fn enable_stress(rt: &mut Runtime) {
    rt.gc_set_stress(true);
}
```

注意：

- `--gc-stress` 在非 debug / 非 `gc_generations` 构建中通常直接拒绝或不存在，不是静默 no-op；
- 非常慢（allocations × heap size），**只跑小脚本、定向用例**，不要当全量回归开关；
- 保留完整输出。失败时记录正在跑的用例、`RUST_BACKTRACE=1` 的栈；
- stress 只负责“更频繁地跑 minor”，不会替代 trace/barrier 建模：validator 只能看到 `trace`
  报告的边。

## 7. 工具局限

- 静态审计：只看 shape，不看时序、`RefCell` 跨 JS、`remove` 后逃逸；**不解析 `GcTrace` 方法体**，
  也**不能验证 owner**。手写 trace 漏字段、owner 传错都不会被它发现。
- 静态审计的 `unknown type` 只是提示，不是“安全”证明；跨行赋值也可能识别不到。
- no-trace 检查（`empty_trace_violations`）只递归本地 `struct`/`enum` 字段；跨 crate 未知类型会跳过，
  `PhantomData`/函数指针不当作拥有的边。
- payload 写在 `mod`/函数内部会直接报错，而不是悄悄漏检。
- 动态 minor：只看实际跑到 minor 的路径、只看 `trace` 报告的边、只在 debug 存在；trace 漏字段时
  validator 也看不到那条边。
- gc stress：只提高 minor 频率，不扩大 `trace` 的可见范围，也不证明未跑到的路径。
- **release 正确性最终依赖 `#[derive(GcTrace)]` + private fields + setter（owner 由 self 推导）+
  guard slots + review，不依赖 validator。**

## 8. 常见错误与排查

规则见 [`gc-safety.md` Rule 5](gc-safety.md#rule-5--old-容器接收-young-值写屏障)；这里只列
表现和定位方式。

| 现象 | 定位 |
|---|---|
| `— missing write barrier` | 后写路径绕过了 setter/guard；或 owner 传错；或容器元素没有 `GcBind` |
| 静态审计报裸字段 | 字段没用 guard slot，也没有合法 exemption；先改代码 |
| 静态审计报 whole-field replacement | `field = WriteBarrier::new(...)`、`*binding = ...` / `mem::replace` 绕过 barrier |
| 静态审计报元素类型缺 `GcBind` | 给元素类型实现 `GcBind`，weak/`#[gc(skip)]` 跳过 |
| 静态审计报 `payload type not parsed` | payload 声明在 mod/函数里（缩进）；移到顶层，不要靠调低数量下限 |
| 静态审计报 `unsafe_empty_gc_trace! type contains GC node` | 删除该宏；类型确实无 GC 节点时才保留，有 GC 字段就改 derive / 手写 trace / `#[gc(skip)]` |
| 静态审计 `⚠ unknown type` | 先确认它是不是 GC 节点：是就按 GC 字段补 guard/trace；否则接受这条非阻塞提示 |
| 静态审计报 stale exemption | 字段已改名/已包裹/删除；enum 字段用 `Type::Variant.field` / `Type::Variant.N` |
| 静态审计绿但 full mark 丢对象 | 手写 `GcTrace` 漏字段；改为 `#[derive(GcTrace)]` + `#[gc(crate_path = zuqe::gc_lite)]` |
| 只有 `--gc-stress` 才崩 | 正常分配频率不足；用定向 minor 测试缩小路径 |
| release 随机崩溃但 debug 不复现 | 漏屏障的静默 UAF；回到静态审计 + 定向 minor + review |
| `gc: bad node` | 先按 [`gc-trace.md`](gc-trace.md#4-排查-trace-问题) 查 trace，再查写入路径 |

提交前请直接对照 [`gc-safety.md` Rule 5](gc-safety.md#rule-5--old-容器接收-young-值写屏障) 的检查项，本文件不再重复 checklist。
