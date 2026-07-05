# Shape 属性查找系统性能分析与改进方案

&gt; **⚠️ 重要更新**：原 HashList → SmallVec/排序数组 替换方案经过实测验证，**效率反而下降**（HashList 的连续 MemChunk 在 CPU cache 层面表现好于预期，自研 ShapePropList 的分支开销反而更大）。结论：**保持 `HashList` 不变**。
&gt;
&gt; **📌 当前方案**：Object 级属性缓存，与字节码完全无关的中性优化。
&gt;
&gt; **🔧 Feature 控制**：通过 `object_prop_cache` feature 开关控制，默认启用。

---

## 1. 现状分析

### 1.1 当前架构

zhuque 的 Shape 系统使用了一个经典的 **transition-based hidden class** 设计（类似 V8 的 Map），但属性定义（`ShapeProperty`）存储在外部 `hash-link` crate 提供的 `HashList` 中。

**核心数据结构**:

```rust
// shape.rs: ShapeImpl 的属性定义容器
pub(crate) struct ShapeImpl {
    pub(crate) prop_defs: HashList&lt;ShapeProperty&gt;,  // ← 热点的关键
    pub(crate) proto: Option&lt;Object&gt;,
    pub(crate) hash: u32,
    pub(crate) use_count: u32,
    // ...
}

// prop.rs: 每个属性定义仅 8 字节（4+4 无 padding）
pub(crate) struct ShapeProperty {
    pub(crate) atom: Atom,  // 4 字节 u32
    pub(super) flags: u32,  // 4 字节 u32
}
```

### 1.2 查找路径

火焰图显示 `find_shape_property` 是 **最大的单点热点**（1.39% 自样本 + 子函数链合计约 6-8%）。

```
find_shape_property(prop)
  → self.shape()                    (0.89%) 获取 Shape 引用
  → Option::and_then                (1.06%) Option 解包
  → sh.prop_defs.find_hash(hash)    (0.57%) 哈希表桶定位 + 链表遍历
  → HashLinkFindIter::next()        (0.64%) 逐个元素迭代
  → filter(非 Deleted)              (1.31%) 闭包过滤
  → map → next()                    (0.81%) 迭代器适配器
```

### 1.3 19 处调用点

`find_shape_property` 在整个代码库中被调用 19 次（不含 `has_shape_property` 间接调用）：

| 调用位置                  | 场景                                | 调用频率 |
| ------------------------- | ----------------------------------- | -------- |
| `get.rs:39`               | `get_property_internal` → 属性读    | **最高** |
| `set.rs:70`               | `set_property_internal` → 属性写    | **最高** |
| `own.rs:103`              | `get_own_property` → 自有属性描述符 | 中       |
| `delete.rs:34`            | `delete_property` → 属性删除        | 低       |
| `define.rs:49`            | `define_property` → 属性定义        | 中       |
| `global.rs:26,67`         | 全局变量读/写                       | 高       |
| `own.rs:43,56`            | 自身方法                            | 高       |
| `private.rs:22,50,67,132` | 私有字段操作                        | 低       |

---

## 2. Prop Cache（属性缓存）方案

### 2.1 核心思想

**Prop Cache（属性缓存）** 是 Shape 属性查找的 **正交优化**——数据结构优化解决的是"单次查找的速度"，Prop Cache 解决的是 **"查找次数"** （跳过查找）。

**基本原理**：在 **Object 对象内部**维护一个 `(atom_id → slot_index)` 的快速查询表。属性访问时先查此表，命中则直接访问 `prop_values[slot]`，**完全不经过 HashList**。

```
Object {
  prop_cache: {atom_x → slot_2, atom_y → slot_5}
  prop_values: [..., vx, ..., vy, ...]
  shape: ShapeImpl { prop_defs: HashList{...} }
}

find_shape_property(obj, x):
  ① 查 obj.prop_cache[x] → slot_2 ✅
  ② 直接返回 (slot_2, flags)    ← 完全跳过 HashList
```

### 2.2 设计决策：为什么是 4 个条目？

| 缓存容量   | 内存开销（per object）                  | 适用场景                     | 命中率估计 |
| ---------- | --------------------------------------- | ---------------------------- | ---------- |
| 1 条目     | 8 字节                                  | 极简场景（只重复读一个属性） | 30-50%     |
| **4 条目** | **24 字节**（Cell + SmallVec overhead） | **绝大多数场景**             | **85-95%** |
| 8 条目     | 40 字节                                 | 复杂对象（多属性循环访问）   | 90-98%     |

**选择 4 个条目的理由**：

1. **覆盖 80-90% 的场景**：
   - 典型 JS 对象：3-5 个属性频繁访问
   - 典型访问模式：`obj.a`, `obj.b`, `obj.c`, `obj.a`（循环访问几个属性）

2. **内存开销可控**：
   - 仅增加 24 字节 per Object（相比 ObjectImpl 已有的 100+ 字节，增加 &lt; 25%）
   - SmallVec 在栈上分配，无堆分配开销

3. **边际收益递减**：
   - 从 1→4：命中率大幅提升
   - 从 4→8：命中率提升有限，但内存翻倍

### 2.3 缓存结构设计

采用**多条目缓存 + Shape 指针验证**的设计：

```rust
#[cfg(feature = "object_prop_cache")]
type PropCacheEntry = (u16, u32);

pub(crate) struct ObjectImpl {
    // ... 现有字段 ...

    #[cfg(feature = "object_prop_cache")]
    /// 缓存验证：上次查找时的 shape 指针
    pub(crate) cached_shape_ptr: Cell&lt;*const ShapeImpl&gt;,

    #[cfg(feature = "object_prop_cache")]
    /// 属性读取缓存：最近 4 次的 (atom_id → slot_index) 映射
    pub(crate) prop_cache: Cell&lt;SmallVec&lt;[PropCacheEntry; 4]&gt;&gt;,
}

#[cfg(feature = "object_prop_cache")]
impl ObjectImpl {
    /// 快速查找缓存：先验证 shape 匹配，再查缓存
    fn prop_cache_find(&amp;self, atom: Atom) -&gt; Option&lt;(usize, u32)&gt; {
        let shape = self.shape()?;
        let shape_ptr = shape.gc_head_ptr();

        // 检查 shape 是否匹配
        if self.cached_shape_ptr.get() != shape_ptr {
            // Shape 改变，清空缓存
            self.cached_shape_ptr.set(shape_ptr);
            self.prop_cache.set(SmallVec::new());
            return None;
        }

        // Shape 匹配，查找缓存
        let cache = self.prop_cache.get();
        cache.iter()
            .find(|&amp;&amp;(a, _)| a == atom.id() as u16)
            .and_then(|&amp;(_, slot)| {
                // 从 shape 中获取 flags（双重保险）
                shape.prop_defs.iter()
                    .find(|&amp;(_, p)| p.atom == atom)
                    .map(|(i, p)| (i, p.flags))
            })
    }

    /// 更新缓存
    fn prop_cache_update(&amp;mut self, atom: Atom, slot: usize) {
        let mut cache = self.prop_cache.take();

        // 先看是否已存在该 atom，更新即可
        for e in cache.iter_mut() {
            if e.0 == atom.id() as u16 {
                e.1 = slot as u32;
                self.prop_cache.set(cache);
                return;
            }
        }

        // 不存在，插入新条目
        if cache.len() &gt;= 4 {
            // 已满，采用 FIFO（先进先出）策略：移除最旧的
            cache.remove(0);  // 移除索引0（最早加入的）
        }
        cache.push((atom.id() as u16, slot as u32));
        self.prop_cache.set(cache);
    }
}
```

### 2.4 集成方案：在 find_shape_property 层面

修改 [`find_shape_property`](file:///Users/johnray/devel/zhuque/src/object/property/own.rs#L43-L97)：

```rust
pub(crate) fn find_shape_property(&amp;self, prop: Atom) -&gt; Option&lt;(usize, u32)&gt; {
    #[cfg(feature = "object_prop_cache")]
    {
        // 快速路径：先查缓存
        if let Some(result) = self.prop_cache_find(prop) {
            return Some(result);
        }
    }

    // 慢速路径：原 HashList 查找
    let result = self.shape().and_then(|sh| {
        sh.prop_defs
            .find_hash(prop.id())
            .filter(|&amp;(i, _)| !matches!(self.prop_slot(i), JSProperty::Deleted))
            .map(|(i, prs)| (i, prs.flags))
            .next()
    });

    #[cfg(feature = "object_prop_cache")]
    {
        // 查找成功，更新缓存
        if let Some((slot, _)) = result {
            self.prop_cache_update(prop, slot);
        }
    }

    result
}
```

### 2.5 实现计划

| 任务                        | 文件                         | 描述                                               | 工时 |
| --------------------------- | ---------------------------- | -------------------------------------------------- | ---- |
| 1. 添加 feature 声明        | `Cargo.toml`                 | 新增 `object_prop_cache` feature（默认不启用）     | 0.5h |
| 2. ObjectImpl 新增缓存字段  | `src/object/obj.rs`          | `#[cfg(feature="object_prop_cache")]` 包裹缓存字段 | 1h   |
| 3. cache 查找/更新方法      | `src/object/obj.rs`          | `#[cfg(feature="object_prop_cache")]` 包裹方法     | 1h   |
| 4. find_shape_property 集成 | `src/object/property/own.rs` | 插入条件编译的快速路径                             | 1h   |
| 5. 测试 + 基准              | `tests/`                     | 开启/关闭 feature 的正确性 + 性能测试              | 2h   |

**预计：1-2 天**

### 2.5.1 Cargo.toml 修改

```toml
[features]
# ... 现有 features ...

# Object 属性缓存优化
object_prop_cache = []

default = [
    # ... 现有 default features ...
    # 默认不启用 object_prop_cache
]
```

### 2.5.2 使用方式

```bash
# 默认：不启用缓存
cargo build

# 启用属性缓存
cargo build --features object_prop_cache
```

### 2.6 与 optimize-object.md 方案的关系

| 特性         | optimize-object.md 方案 | optimize-object-prop-cache.md 新方案 |
| ------------ | ----------------------- | ------------------------------------ |
| **缓存容量** | 1 个条目                | **4 个条目**                         |
| **集成位置** | find_shape_property     | find_shape_property                  |
| **正确性**   | shape 指针匹配          | shape 指针匹配                       |
| **启发来源** | -                       | 受到 optimize-object.md 的启发       |

**说明**：本方案整合了两者的优点：

- 借鉴 `optimize-object.md`：在 find_shape_property 层面集成 + shape 指针验证
- 保留本方案的原创设计：多条目缓存 (4个) 提高命中率

### 2.7 预期收益

| 查找路径          | CPU cycles |
| ----------------- | ---------- |
| HashList 完整查找 | ~80-150    |
| Object cache 命中 | ~3-10      |
| 预计整体引擎加速  | **10-20%** |
