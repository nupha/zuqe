# 编写 native class

> 覆盖宿主/引擎内建 native class 的标准模式。**zapi 第三方扩展**定义 class 走 zapi ABI（`ZqClassDef`/`ZqFieldDef`、由 host 注册），不是 `def_native_object!` 直接路径——见文末边界与 [native-module.md](native-module.md)。

## `def_native_object!`：宿主类（host/external crate）

源码：`src/class/native.rs`。宿主类通过 linkme `HOST_CLASS_DEFS` 在引擎初始化时分配 class id（`Type::class_id()` 在 init 前为 0）。在 context 中使用前必须 `use_class`。

```rust
use zuqe::{
    CallContext, ClassCtorDef, JSResult, JSValue, NativeObject, ScopedContext,
    def_native_object,
    gc_lite::GcTrace,
};

#[derive(Debug, GcTrace)]
#[gc(crate_path = zuqe::gc_lite)]
pub struct FooPayload {
    value: u32,
    // GC 节点字段由 derive 自动 trace；可变 GC 字段还需 WriteBarrier
    // （细节见 gc-trace.md / gc-write-barrier.md）
    cached: Option<zuqe::Object>,
}

// 绝大多数情况用 derive：字段和 enum 变体自动覆盖，新增字段不会漏。
// 只有特殊 trace 逻辑才手写 impl GcTrace；无 GC 节点的 payload 见下方专节。

fn js_foo_ctor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    // new_target = cc.this() / constructor 协议按引擎约定；示例用 cc.this()
    JSFoo::new_instance(ctx, cc.this(), FooPayload { value: 0, cached: None })
        .map(JSValue::from)
}

pub const FOO_PROTO: &[zuqe::FieldDef] = &[
    // FieldDef::FUNC / GETSET / ... 见 native-function.md
];

def_native_object!(
    "Foo",
    JSFoo,        // #[repr(transparent)] wrapper over Object
    FooPayload,   // 或 () 表示无 payload；零尺寸 payload 一律用 ()
    FOO_PROTO,
    Some(ClassCtorDef::new(js_foo_ctor, 0, false)), // (ctor, length, call_as_func)
    // extends: JSBase::class_id,                    // 可选：声明父类（见继承）
);
```

`ClassCtorDef::new(callback, length, call_as_func)`：

- `callback`: 构造回调，签名同 `CallbackFn`。
- `length`: `Function.length`。
- `call_as_func`: `true` 时允许不带 `new` 直接调用（`Callback::ConstructorOrFunc`）；
  `false` 时只能 `new`（`Callback::Constructor`）。
- 链式配置：`.expose_to_global()` 自动装到全局；`.magic(i16)` 设置 magic；
  `.entries(&'static [FieldDef])` 安装静态成员。

`def_native_object!` 会为带 payload 的宿主类接上 `drop_payload_fn::<P>` 与
`user_payload_trace::<P>`；由于后者要求 `P: GcTrace`，**payload 类型没有
`GcTrace` 会在宏调用处直接编译失败**。`extends:` 只声明父类，子类 payload 不会
共享父类的 trace。

### Context 初始化

```rust
ctx.use_class(JSFoo::class_id())?;
// 之后可通过 ctx.class_proto/constructor 等按需安装到全局
```

`use_class`/`use_classes` 会按需扩展 class slots 并初始化 class 定义；带 `extends` 的宿主类按引擎实现建立继承链（参考 `src/class/native.rs` 测试 `host_class_extends_links_both_chains`）。

### Payload 访问

宏生成：

- `payload() -> Option<&Payload>` / `payload_mut() -> Option<&mut Payload>`
- `take_payload() -> Option<Box<Payload>>`（取走后 payload slot 为空）
- `payload_of(&JSValue) -> Option<&Payload>` / `payload_mut_of(&mut JSValue)` /
  `take_payload_of(&mut JSValue)`

访问前宏/NativeObject 已校验 class_id。`payload_raw`/`replace_payload` 属底层能力，
优先用生成的类型安全方法。

### 创建实例

- 构造函数内：`Self::new_instance(ctx, cc.this(), payload)`（`cc.this()` 是
  `new_target`）。
- 构造函数外（工厂/模块导出）：`Self::new_instance(ctx, JSValue::UNDEFINED, payload)`；
  引擎会用类自己的 prototype 创建实例。

```rust
impl JSFile {
    pub fn create_file_instance(ctx: &mut ScopedContext, fh: std::fs::File)
        -> Result<Object, i32> {
        Self::new_instance(ctx, JSValue::UNDEFINED, RefCell::new(FileDesc::File(fh)))
            .map(Into::into)
    }
}
```

`FieldDef::LAZY_VALUE` 可以在模块导出时构造实例：

```rust
FieldDef::LAZY_VALUE("in", |cx| {
    JSFile::create_stdin_instance(cx).map(JSValue::from)
}, JS_PROP_ENUMERABLE)
```

### 通过 `FieldDef::CLASS` 导出构造器

```rust
static FILE_MODULE_ENTRIES: &[FieldDef] = &[
    FieldDef::CLASS("File", || JSFile::class_id(), JS_PROP_C_W_E),
];
```

`FieldDef::CLASS` 接收 `fn() -> ClassID`，模块 init 时按 class_id 查找并暴露构造器。

### 无 payload

```rust
def_native_object!("Bar", JSBar, (), BAR_PROTO, None);
// JSBar::new_instance(ctx, new_target) -> Result<Self, i32>
```

零尺寸/无状态的 payload 不需要存在：直接用 `()`，不要写 `struct BarPayload;` 再给它一个空
`GcTrace`。例：`TextEncoder` 没有任何实例状态，就用 `()` 形式，省掉每次实例化的 `Box` 分配和
`payload_drop`。

### 无 GC 节点的 payload

payload 有数据，但传递闭包内**没有** `JSValue`/`Object`/`GcRef`/`WriteBarrier` 等 GC 节点时，按字段
是否实现 `GcTrace` 选择：

```rust
// 字段都可 trace（String/Vec<u8>/Arc<[u8]>...）：仍优先 derive。
// 当前生成空 trace，以后新增 GC 字段会自动跟上。
#[derive(GcTrace)]
#[gc(crate_path = zuqe::gc_lite)]
pub(crate) struct BlobPayload {
    bytes: Arc<[u8]>,
    type_: String,
}

// 字段不实现 GcTrace（raw handle、第三方容器、tokio Mutex...）：
// 用显式空 trace 宏，替代手写空 impl。
pub(crate) struct IoHandlePayload {
    file: tokio::fs::File,
    kind: HandleKind,
}
zuqe::gc_lite::unsafe_empty_gc_trace!(IoHandlePayload);
```

`unsafe_empty_gc_trace!` 是对**整个类型**的断言，不是消编译错误的工具：

- 它要求类型传递闭包内没有任何 GC 节点；以后加入 `JSValue`/`Object`/`WriteBarrier` 字段，必须删除宏
  并补 trace，否则就是 UAF。
- 只有一两个字段不实现 `GcTrace` 时，优先 `#[derive(GcTrace)]` + `#[gc(skip)]`，粒度更小、更安全。
- `gc_audit` 会递归检查被宏标记的类型：可达字段里出现 GC 节点（包括包在 `WriteBarrier` 里的）会直接
  失败。

## 构造、方法、属性、原型

- **原型方法/属性**：通过 `proto_entries`（`&[FieldDef]`）声明；`FieldDef::FUNC`/`GETSET`/常量等。
- **构造器**：`ClassCtorDef::new(fn, length, is_constructor)`；构造回调内创建实例并附加 payload。
- **`this` 类型检查**：方法回调中用 `NativeObject::from_value`/`from_object` 或 `JSFoo::payload_of(&cc.this())` 等校验，失败抛 `TypeError`。
- **属性标志**：按 ECMA/WebIDL 语义选择 `JS_PROP_*`；不要随意放宽 configurable/writable/enumerable。

## 类继承：**只有一条落地路径**

这是项目硬约束（`AGENTS.md` / 任务要求），用于避免多套建链逻辑漂移：

1. **先按根类建**，再建立继承关系；不要在构造过程中手工拼链。
2. **宿主自带类**：使用 `def_native_object!(..., extends: Parent::class_id)` 声明父类，并通过 `use_class` 初始化（引擎按统一机制链接）。
3. **引擎内建/内核原语**：继承关系由 `ScopedContext::link_class_extends` / `init_class_proto_extends`（及相关 `bind_ctor_proto*`）建立——这些是 `pub(crate)` 原语（`src/class/using.rs`），是**引擎唯一**落地实现；内建类的 `class_init` 应复用它们，不另起炉灶。
4. **禁止**手写 `obj.set_prototype(...)` 来建立 class prototype/constructor 继承链（除了引擎原语内部；业务/绑定代码不得绕过）。
5. **不开放 InitFn 给第三方**。第三方扩展按其受众走 zapi 支持的范式；不要把内建 init 机制暴露/照搬给第三方。
6. **宿主/内建 与 zapi 第三方范式不混用**：同一需求选型时先定路径（见 [workflow-checklist.md](workflow-checklist.md)）。

> 若你不确定某继承写法是否合规：能用 `def_native_object! extends` 就用；不能用时查 `src/class/using.rs` 与现有内建模式，**不要**发明新建链方式。

## GC：payload 设计（引用）

> 规则见 [`gc-safety.md`](gc-safety.md#2-gc-rules唯一规则来源) §2 Rule 0-5；trace 示例/反例见
> [`gc-trace.md`](gc-trace.md#2-实现示例)；屏障实现与工具见 [`gc-write-barrier.md`](gc-write-barrier.md#2-实现模式)。

写 class/payload 时重点对照 Rule 1（trace）与 Rule 5（后写 barrier）；`extends:` 子类、
weak/`#[gc(skip)]`、容器元素 `GcBind` 也按对应规则处理。payload 无自动 re-trace fallback
（zapi 实例的 per-minor payload 重扫是引擎对 zapi 路径的补偿，不适用于宿主 payload）。

无 GC 节点的 payload 按“存在形式”选最小方案：零尺寸/无状态 → `()`；非零尺寸且字段不实现
`GcTrace` → `unsafe_empty_gc_trace!`；字段都可 trace → `#[derive(GcTrace)]`。不要把空 `impl GcTrace`
当成默认写法。

## zapi 第三方 class（边界说明）

zapi 扩展通过 `ZqClassDef`/`ZqFieldDef`/body trampoline 描述类与成员，由 `src/zapi/host.rs` 加载并注册；GC 追踪通过 class `gc_trace` 回调，并由引擎对 **old zapi 实例**在每次 minor 做 payload 重扫（补偿 zapi 无 barrier ABI 的限制）——这仍要求 `gc_trace` 回调报告其持有的全部 GC 字段。该路径是 **ABI/FFI**，不使用 `ScopedContext`/`FieldDef`/`def_native_object!` 宿主宏。第三方不得依赖/获得 InitFn 等内建特权。

选型时若需求是“宿主二进制内嵌 zuqe 并加内建能力”，走宿主路径；若是“可加载 cdylib 扩展”，走 zapi 路径并遵循 zapi 约束。
