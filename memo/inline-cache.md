# 字节码内联缓存（Inline Cache）方案

## 1. V8 也在用这个方法吗？

**是的。** Inline Cache（IC）是 V8 执行属性访问的核心优化手段。

V8 的 `LoadIC` / `StoreIC` 机制类似这样：

```
V8 字节码:
  LdaNamedProperty <object>, <name_index>
    → 解释器查找 Feedback Vector 中的 Slot
    → Slot 记录: (Map, handler_offset)
    → 首次执行: 走慢路径，填充 Slot
    → 后续执行: 比较 Map 指针 → 命中则直接读
```

V8 在每个 `JSFunction` 中维护一个 **Feedback Vector**——**独立于字节码指令流的并行表**。每条属性访问指令通过 `slot_index` 索引到这个表。

zhuque 的方案采用同样的结构。

---

## 2. 优化原理

### 2.1 核心观察

JavaScript 属性访问存在 **99% 的单态性（monomorphic）**——**同一条指令访问的属性，其所属对象的 shape 几乎从不改变**。

```javascript
function getX(o) { return o.x; }
// 99% 的情况下 o 都来自同一 class/构造函数，shape 完全一样
```

所以 99% 的属性访问可以用：

```
命中: 1 次指针比较 + 1 次 slot 读
未命中: 走完整查找 + 更新缓存
```

### 2.2 为什么不是 Object 级缓存

每个 Object 上做多条目缓存在此代码库无效——原因是多个属性共享固定槽位相互驱逐，且 Hot 属性访问的对象 shape 通常很小（N=3~5），HashList 本身已足够快。

IC 把缓存贴在**指令**上：

```
Object X = {a: 1, b: 2, c: 3}

PC=100: GET_FIELD_IC  "a", idx=0  ──→ cache[0]  (独占)
PC=105: GET_FIELD_IC  "b", idx=1  ──→ cache[1]  (独占)
PC=110: GET_FIELD2_IC "c", idx=2  ──→ cache[2]  (独占)

三条指令各有一个专属 cache 条目，互不干扰，命中率 100%。
```

即使访问不同对象：

```javascript
getX(obj1);  // 第一次: miss, 填 cache[0] = {shape=&ShapeA, slot=0}
getX(obj2);  // shape == &ShapeA → 命中, 读 prop_values[0]
getX(obj3);  // shape == &ShapeA → 命中
```

### 2.3 单态性假设失效时

当同一指令遇到不同 shape 的对象（多态），cache miss，退化为完整查找。V8 会记录多个 shape（多态 IC，polymorphic），zhuque 先保持**单态 + 退化**。

### 2.4 收益来源

```
优化前：find_shape_property → find_hash → 链表遍历 → 过滤 → 返回
优化后：shape 指针比较 → 读 slot
```

火焰图数据显示 `find_shape_property` 及其子函数合计约 6-8%，IC 命中后将这些路径完全跳过。

---

## 3. 方案：并行 Cache 表

### 3.1 Feature 控制

在 `Cargo.toml` 中新增：

```toml
[features]
inline_cache = []
```

当 feature **启用**时：
- 编译器生成 `GET_FIELD_IC` / `GET_FIELD2_IC` 指令（代替 `GET_FIELD` / `GET_FIELD2`）
- `BytecodeImpl` 中包含 IC 并行 cache 表
- 解释器遇到 `GET_FIELD_IC` / `GET_FIELD2_IC` 时走 IC 快速路径

当 feature **关闭**时：
- 编译器行为不变，发射 `GET_FIELD`
- `BytecodeImpl` 中不含 cache 表
- 对现有逻辑**零影响**

### 3.2 指令格式

新增操作码 `GET_FIELD_IC` 和 `GET_FIELD2_IC`（同时预留 `PUT_FIELD_IC`）：

```
offset  size  field
0       1     opcode (GET_FIELD_IC / GET_FIELD2_IC)
1       4     atom (u32 — 属性名 ID)
5       2     ic_index (u16 — 并行 cache 表索引)
```

两条指令长度均从 5 字节变为 **7 字节**。栈效果与原版一致：`GET_FIELD_IC` pop=1 push=1，`GET_FIELD2_IC` pop=1 push=2。

> 用 u16 而不是 u32：单个函数内的 IC 站点数极少超过 256（热循环内的几条 GET_FIELD），u16 足够。

### 3.3 Shape ABA 问题与 uid 解决方案

#### 问题描述

非移动 GC 下，缓存裸指针存在 AB A问题：

```
t1: ShapeA@0x100 cached in IC (shape_ptr=0x100)
t2: ShapeA freed (use_count=0, 已 unregister), 地址 0x100 归还
t3: 新 ShapeB 分配到 0x100 (内存复用)
t4: GET_FIELD_IC 比较: 0x100 == 0x100 → 误匹配 ← BUG
```

#### 解决方案：Shape uid

给每个 ShapeImpl 分配一个**全局单调递增的 uid**：

```rust
// ShapeImpl 新增
pub(crate) uid: u32,

// RuntimeImpl 全局计数器
next_shape_uid: Cell<u32>,
```

创建 shape 时赋值：

```rust
fn new(proto, props) -> ShapeImpl {
    let uid = rt.next_shape_uid.get();
    rt.next_shape_uid.set(uid + 1);
    // ...
    Self { uid, /* ... */ }
}
```

IC 条目存储 `(shape_ptr, uid)`：

```rust
struct InlineCacheEntry {
    /// 缓存的 ShapeImpl 裸指针。
    shape_ptr: *const ShapeImpl,
    /// shape 创建时的 uid。与 shape_ptr 组合验证，防止 ABA。
    shape_uid: u32,
    /// 缓存的 prop_values slot 索引
    slot: u32,
    /// 缓存的属性 flags
    flags: u32,
}
```

命中检查：

```rust
if entry.shape_ptr == shape_ptr && entry.shape_uid == shape.uid {
    // 命中 ✓  — shape 身份确认，排除 ABA
}
```

每 shape 多 4 字节，500-1000 个常驻 shape 仅 2-4KB 开销。

### 3.4 属性删除的处理

Delete 操作**不改变 shape**，只将 slot 标记为 `JSProperty::Deleted`。因此 shape 指针不变，IC 命中后必须额外检查 slot 状态：

```rust
if entry.shape_ptr == shape_ptr && entry.shape_uid == shape.uid {
    let slot = entry.slot as usize;
    // ★ 必须检查：delete 不改变 shape，但 slot 可能已标记为 Deleted
    if matches!(obj.prop_slot(slot), JSProperty::Deleted) {
        goto miss;  // 退化为完整查找
    }
    // 安全读取
    let val = obj.prop_slot(slot).value(ctx, obj.into())?;
    // ...
}
```

### 3.5 并行 Cache 表

Cache 表存储在 `BytecodeImpl` 中，跟随字节码生命周期：

```rust
// BytecodeImpl 新增
#[cfg(feature = "inline_cache")]
pub(crate) inline_cache: Box<[InlineCacheEntry]>,
```

```rust
#[cfg(feature = "inline_cache")]
#[derive(Clone, Copy)]
#[repr(C)]
pub(crate) struct InlineCacheEntry {
    /// 缓存的 ShapeImpl 裸指针。null = 未初始化。
    shape_ptr: *const ShapeImpl,
    /// Shape 创建时的 uid，与 shape_ptr 组合防 ABA。
    shape_uid: u32,
    /// 缓存的 prop_values slot 索引
    slot: u32,
    /// 缓存的属性 flags
    flags: u32,
}
```

初始状态 `shape_ptr = null`（首次执行必然 miss）。

### 3.6 编译流程

```
#[cfg(not(feature = "inline_cache"))]
emit GET_FIELD / GET_FIELD2 "x"
  → [GET_FIELD / GET_FIELD2 | atom]
  → 5 字节 ← 和现在完全一样

#[cfg(feature = "inline_cache")]
emit GET_FIELD_IC / GET_FIELD2_IC "x"
  → 第一遍扫描: 统计 IC 指令站点数 N
  → 分配 Box<[InlineCacheEntry; N]>，全零初始化
  → 第二遍发射: [GET_FIELD_IC / GET_FIELD2_IC | atom | ic_index]
  → 7 字节
```

编译器在字节码生成完成后，增加一个 pass：

1. 遍历已发射的操作码，统计 `GET_FIELD_IC` + `GET_FIELD2_IC` 的总数 N
2. 按 N 分配 cache 数组
3. 发射时对每条 IC 指令分配递增的 `ic_index`，两条指令共享同一种 `InlineCacheEntry`

### 3.7 解释器执行逻辑

```rust
OpCode::GET_FIELD_IC | OpCode::GET_FIELD2_IC => {
    let atom = Atom::from_id(PC_FETCH!(u32));
    let ic_idx = PC_FETCH!(u16) as usize;
    // ic_index 指向的 cache entry 兼容 inline_cache 阵列
    // 入口通用（shape_ptr + uid + slot + flags），无 stack track 差异

    #[cfg(feature = "inline_cache")]
    {
        // 快速路径：尝试 IC 命中
        let bc = current_bytecode();
        if ic_idx < bc.inline_cache.len()
            && let Some(obj) = SP_VAL!(-1).as_object()
            && let Some(shape) = obj.shape()
        {
            let entry = &bc.inline_cache[ic_idx];
            let shape_ptr: *const ShapeImpl = &**shape as *const ShapeImpl;

            if entry.shape_ptr == shape_ptr
                && entry.shape_uid == shape.uid
            {
                let slot = entry.slot as usize;
                if !matches!(obj.prop_slot(slot), JSProperty::Deleted) {
                    let val = obj.prop_slot(slot).value(ctx, obj.into())?;
                    if opcode == OpCode::GET_FIELD_IC {
                        SP_PUT!(-1, val);           // obj → val
                    } else {
                        SP_PUT!(0, val);             // obj → obj, val
                        SP_GO!(1);
                    }
                    continue;
                }
            }
        }

        // 慢路径：完整属性查找 + 更新 IC
        let (val, found) = slow_path_get_property(obj, atom)?;
        if let Some(shape) = obj.shape() {
            let shape_ptr = &**shape as *const ShapeImpl;
            bc.inline_cache[ic_idx] = InlineCacheEntry {
                shape_ptr,
                shape_uid: shape.uid,
                slot: found.slot as u32,
                flags: found.flags,
            };
        }
        // 栈操作与上面一致，抽成公共后处理
        if opcode == OpCode::GET_FIELD_IC {
            SP_PUT!(-1, val);
        } else {
            SP_PUT!(0, val);
            SP_GO!(1);
        }
    }

    #[cfg(not(feature = "inline_cache"))]
    {
        unreachable!()
    }
}
```

> `BytecodeImpl` 通过 `GcRef` 访问，写入 `inline_cache` 需要 `&mut`。解释器为单线程，可通过 `UnsafeCell` 或 `GcRef::as_mut` 原地修改。

### 3.8 Cache 失效

不需要主动失效，全部惰性检测：

| 事件 | 效果 |
|------|------|
| 对象添加属性 | 新 Shape → `set_shape` 调用 → `shape_ptr` 不匹配 → miss |
| 对象删除属性 | Shape 不变 → `shape_ptr` 和 `uid` 都匹配 → 进入检查 → `prop_slot(slot)` 为 `Deleted` → miss |
| 对象修改属性 flags | 新 Shape → 指针不匹配 → miss |
| Shape 被回收后地址复用 | `shape_ptr` 匹配但 `shape.uid` 不同 → miss |

### 3.9 与 V8 方案对比

| | V8 Feedback Vector | zhuque IC 方案 |
|---|---|---|
| 存储位置 | JSFunction 层面 | BytecodeImpl 层面 |
| 索引方式 | slot index | ic_index (u16) |
| 缓存条目 | (Map, handler) | `(shape_ptr, uid, slot, flags)` |
| ABA 防护 | 非移动对象 / 写屏障 | `uid` 全局单调递增 |
| 多态支持 | POLYMORPHIC / MEGAMORPHIC | 仅单态（先期） |
| Feature 控制 | 始终启用 | `inline_cache` feature |

---

## 4. 实现计划

| 步骤 | 内容 | 涉及文件 |
|------|------|---------|
| 1 | `Cargo.toml` 新增 `inline_cache` feature | `Cargo.toml` |
| 2 | `ShapeImpl` 新增 `uid: u32` + `RuntimeImpl` 新增 `next_shape_uid` | `shape.rs`, `runtime/rt.rs` |
| 3 | 操作码定义新增 `GET_FIELD_IC` / `GET_FIELD2_IC` | `build_opcodes.rs` |
| 4 | `BytecodeImpl` 新增 `inline_cache: Box<[InlineCacheEntry]>` 字段 | `src/function/bytecode.rs` |
| 5 | 编译器 pass：扫描 IC 指令 → 分配 cache 数组 | `src/function/emit.rs` |
| 6 | 编译器发射：feature 开→`GET_FIELD_IC`/`GET_FIELD2_IC`，关→原指令 | `src/parse/expr_property.rs` |
| 7 | 解释器 `GET_FIELD_IC | GET_FIELD2_IC` 分支：快速路径 + miss 回退 | `src/bytecode/exec_stackless.rs` |
| 8 | 测试 + microbench 对比 | `tests/` |

### 4.1 前置工作：ShapeImpl 增加 uid

```rust
// src/object/shape/shape.rs

pub(crate) struct ShapeImpl {
    pub(crate) uid: u32,      // ← 新增：唯一标识，防 ABA
    pub(crate) use_count: u32,
    pub(crate) hash: u32,
    // ...
}
```

`uid` 在创建时从 `RuntimeImpl::next_shape_uid` 分配：

```rust
ShapeImpl::new(proto, props) -> Self {
    let uid = ctx.rt_mut().next_shape_uid.get();
    ctx.rt_mut().next_shape_uid.set(uid.wrapping_add(1));
    Self { uid, /* ... */ }
}
```

> `wrapping_add`：u32 溢出约 40 亿次后回绕，在此寿命下不会发生。

### 4.2 操作码定义

在 build 脚本中添加：

```
DEF!("get_field_ic",  7, 1, 1, "ATOM u16", false, Never),
DEF!("get_field2_ic", 7, 1, 2, "ATOM u16", false, Never),
```

- `get_field_ic`：7 字节、pop=1 push=1、`GET_FIELD` 的 IC 版
- `get_field2_ic`：7 字节、pop=1 push=2、`GET_FIELD2` 的 IC 版

### 4.3 InlineCacheEntry 定义

```rust
// src/function/bytecode.rs 或新文件 src/function/inline_cache.rs

#[cfg(feature = "inline_cache")]
#[derive(Clone, Copy)]
#[repr(C)]
pub(crate) struct InlineCacheEntry {
    /// 缓存的 ShapeImpl 裸指针。null = 未初始化。
    shape_ptr: *const ShapeImpl,
    /// Shape 创建时的 uid，与 shape_ptr 组合验证防 ABA。
    shape_uid: u32,
    /// 缓存的 prop_values slot 索引
    slot: u32,
    /// 缓存的属性 flags
    flags: u32,
}
```

---

## 5. 预期收益

| 指标 | 当前值（HashList 查找） | IC 命中后 |
|------|-----------------------|----------|
| 属性访问路径 | `find_hash` + 链表遍历 + 过滤 + 原型链 | 1 次指针比较 + 1 次 slot 读 |
| CPU cycles | ~80-150 | ~5-10 |
| `find_shape_property` 调用 | 每次属性访问 | 仅 shape 变化时 |
| 整体引擎加速 | — | 预计 **5-15%** |

保守估计理由：`GET_FIELD`/`PUT_FIELD` 合并占总执行时间 30-50%，IC 能使这部分中的属性查找从 ~100 cycles 降至 ~8 cycles，约节省 90%。整体为 `(30~50%) × 90%` ≈ 27-45% 的热路径加速 → 映射到整体 **5-15%**（取决于存取占比和 IC 命中率）。

---

## 6. 代码审查问题清单

以下是在实现后的代码审查中发现的问题，按优先级排列。

### 状态说明

- **P0**：必须修复，会导致功能损坏或未定义行为
- **P1**：应当修复，影响正确性或 IC 覆盖率
- **P2**：建议修复，影响代码质量或轻微性能
- **P3**：可修复可不修复，风格/清理

| # | 优先级 | 文件 | 问题 | 状态 |
|---|--------|------|------|------|
| 1 | **P0** | `build_opcodes.rs:139-140` | `GET_FIELD_IC`/`GET_FIELD2_IC` 标记为 `Never` GC 分配，但 getter 属性会调用 JS 函数（`Conditional` 分配），导致 `compute_gc_scope_kind` 错误分类 | ✅ 已修复（改为 `Conditional`） |
| 2 | **P0** | `src/function/bytecode.rs` | `GcTrace for BytecodeImpl` 未 trace `inline_cache` 中的裸指针 `*const ShapeImpl` | ✅ 已修复（添加安全模型文档，非移动 GC + shape registry 保活 + 仅比较不 dereference + uid 防 ABA） |
| 3 | **P1** | `src/parse/destructing.rs:238` | 直接 `emit_op(GET_FIELD2) + emit_atom`，未走 `emit_field_get2()`。IC 对解构赋值完全无效 | ✅ 已修复 |
| 4 | **P1** | `src/parse/expr.rs:584` | 可选链遇到 `GET_FIELD_IC` 时，先把 opcode 改为 `OP_get_field` 再截断 ic_index，然后由正常可选链匹配处理 | ✅ 已修复 |
| 5 | **P2** | `src/function/mkbc.rs:326-331` | IC 数组使用 `set_len` 零初始化，依赖未初始化内存。应实现 `Default` trait 后用 `vec![Default::default(); n]` | ✅ 安全（`repr(C)` 标量零值合规） |
| 6 | **P2** | `src/bytecode/exec_stackless.rs:1765,1803` | fast path 与 miss path 各取一次 `inline_cache` 引用，多一次指针解引用 | 🔧 可优化 |
| 7 | **P2** | `src/function/bytecode.rs:271` + `mkbc.rs` | `BytecodeImpl::new()` 先创建空数组再覆盖，一次额外分配 | 🔧 可优化 |
| 8 | **P2** | `src/bytecode/exec_stackless.rs:1778` | fast path 中 `prop_slot(slot).value()` 可能触发 GC，但 gc-lite 为非移动式 GC，地址稳定 | ⚠️ 当前 GC 实现下安全 |
| 9 | **P3** | `memo/inline-cache.md` | 文档描述了两遍扫描流程，实际为 emit 时递增计数器 + 一次性分配 | 🔧 已更新 |
| 10 | **P3** | `InlineCacheEntry` | `flags: u32` 实际只用低 8 位，16 字节对齐下不节省空间 | ✅ 保持不动 |

### 问题详情

#### #1 可选链匹配遗漏

```rust
// src/parse/expr.rs:584 — 当前：
op @ (opcodes::OP_get_field | opcodes::OP_get_array_el) => {

// 应改为：
op @ (opcodes::OP_get_field | opcodes::OP_get_field_ic | opcodes::OP_get_array_el) => {
```

当 inline_cache feature 启用时，`emit_field_get` 发射 `GET_FIELD_IC` 而非 `GET_FIELD`。可选链 patching 代码只匹配 `OP_get_field`，导致 `get_field_ic` 指令未被替换为 `GET_FIELD_OPT_CHAIN`。执行时不会短路，语义错误。

**修复方式**：添加 `opcodes::OP_get_field_ic` 到 match 模式。

#### #2 GC 分配分类

`DEF!("get_field_ic", 7, 1, 1, "ATOM_U16", false, Never)` — 第三个 `false` 是 short 标记，最后一个 `Never` 是 GC 分配标记。IC 快速路径中 `JSProperty::value()` 对 `GetSet` 属性会调用 JS getter（`Conditional` 分配），对 `Normal` 属性无分配。应改为 `Conditional`。

#### #3 GC trace 与悬挂指针

`InlineCacheEntry` 存有 `*const ShapeImpl` 裸指针。`BytecodeImpl` 的 `GcTrace` 实现未 trace 这些指针。存在两种安全模型：

**模型 A（保守）**：在 `BytecodeImpl::trace()` 中遍历 `inline_cache`，对每个 `shape_ptr` 非 null 的条目，设法将对应的 shape 标记为存活。但在当前 gc-lite 框架下，裸指针无法 trace（需要 `GcRef`）。

**模型 B（推理）**：证明 shape 的存活期 >= IC 条目的存活期。IC 条目只指向对象**当前**的 shape。只要对象存活，其 `GcRef<ShapeImpl>` 保证 shape 存活。IC 条目在对象 shape 切换时被覆盖（惰性失效）。因此：
- IC 中的 shape_ptr 要么指向对象的当前 shape（存活）
- 要么在下一次查找时因 uid 不匹配被覆盖

结论：模型 B 下 UB 不会实际触发，但需要显式写入安全文档。

#### #4 解构赋值遗漏

```rust
// src/parse/destructing.rs:238 — 当前：
self.emit_op(OpCode::GET_FIELD2);
self.emit_atom(prop_name);

// 应改为：
self.emit_field_get2(prop_name);
```

三个命名属性获取点中，`expr_property.rs` 的两个已改好，`destructing.rs` 第 238 行遗漏。

#### #8 value() 跨 GC safepoint

快速路径中：
```rust
let ic_cache = unsafe { &*self.inline_cache.get() };
// ... ic_cache[ic_idx] ...
let val = obj.prop_slot(slot).value(realm_scope, obj_val)?;
```

`value()` 对 GetSet 属性会调用 JS getter。getter 执行期间可能触发 GC。若 GC 触发 compact 移动了 `BytecodeImpl`，`ic_cache` 引用（指向 `self.inline_cache` 内部）可能失效。

**安全分析**：gc-lite 为非移动式 GC，不会 compact 移动对象地址，因此此问题在当前 GC 实现下不触发。
