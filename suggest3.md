# 统一 Builtin Class 与 User Native Class 的注册范式

> **状态: Phase 0-6 已完成**
> Phase 6 清理已在 commit 91e9e2a7 及之前完成。

## 1. 统一后的设计

### 1.1 核心思路

扩展 `def_native_object!` 宏，增加 `builtin` 标记分支。builtin class 与 user class 使用**相同的宏语法**进行定义，区别仅在于：

- builtin class 多传 `class_id: (JS_CLASS_XXX, Atom::CONST_XXX)` 预分配参数
- user class 不传这两个参数（运行时分配）

两者注册、实例创建、释放**全部走同一路径**。

### 1.2 统一后的宏语法

```rust
// ── Builtin Class（三种变体）──

// 1) 有 payload + proto/ctor:
def_native_object!(builtin, "Boolean", CLASS_DEF_BOOLEAN,
    class_id: (JS_CLASS_BOOLEAN, Atom::CONST_Boolean),
    bool,                       // payload 类型 (无 payload 则 ())
    js_boolean_proto_funcs,
    Some(ClassCtorDef::new(js_boolean_constructor, 1, true).expose_to_global())
);

// 2) 有 init_fn（自定义初始化）:
def_native_object!(builtin, "TypedArray", CLASS_DEF_UINT8_ARRAY,
    class_id: (JS_CLASS_UINT8_ARRAY, Atom::CONST_Uint8Array),
    TypedArrayPayload,          // payload 类型
    init_fn: TypedArray::class_init
);

// 3) 无 payload + 有 on_call:
def_native_object!(builtin, "Function", CLASS_DEF_C_FUNCTION,
    class_id: (JS_CLASS_C_FUNCTION, Atom::CONST_Function),
    (),                         // 无 payload
    &[],
    None,
    on_call: NativeFunction::class_call
);

// 可选参数: on_finalize:, exotic:, on_call:, extends:
// on_class_register:, on_class_unregister:, init_fn:

// ── User Native Class（不变）──
def_native_object!("Emitter", JSEmitter, EmitterData, CLASS_EMITTER_PROTO,
    Some(ClassCtorDef::new(js_emitter_constructor, 0, false).expose_to_global()));
```

### 1.3 统一后的注册流程

```
RuntimeImpl::new():
    rt.init_const_atoms();
    register_all_builtin_classes(&mut rt)?;  // 由 build_stdclass.rs 生成

register_all_builtin_classes() (生成的函数):
    fn register_all_builtin_classes(rt: &mut RuntimeImpl) -> Result<(), i32> {
        // 迁移后的类: 调用自身的 register() 方法
        crate::builtins::object::CLASS_DEF_OBJECT::register(rt)?;
        crate::builtins::boolean::CLASS_DEF_BOOLEAN::register(rt)?;
        // ...
        // 未迁移的类 (如有): 走 register_builtin_class 路径
        // rt.register_builtin_class(JS_CLASS_XXX, CLASS_DEF_XXX)?;
        Ok(())
    }
```

### 1.4 统一后的释放路径

`ObjectImpl::finalize()` 中统一为**一条**路径，不再区分 builtin/user：

```rust
// 统一 finalize
if let Some(f) = class_def.on_finalize {
    f(&mut *rt, ptr, class_id as usize, id);
}
// 统一 free
if let Some(free) = class_def.payload_drop {
    unsafe { free(ptr) };
}
```

## 2. 已完成的实现

### Phase 0: Finalize 路径统一 ✅

**文件: `src/class/mod.rs`, `src/class/class_def.rs`, `src/object/obj.rs`**

1. 删除 `ClassFinalizeFn` 类型（旧的 3-arg `fn(rt, isa, id)`）
2. 将 `ClassFinalizeOpaqueFn` 重命名为 `ClassFinalizeFn`（统一 4-arg 签名）
3. 删除 `ClassDef::on_finalize()` 方法（旧 3-arg），`on_dispose()` 重命名为 `on_finalize()`
4. `ObjectImpl::finalize()` 删除 `if class_id >= USER_CLASS_ID_START` 分支
5. 迁移 `ArrayBuffer::class_finalize` 签名加 `_class_id` 参数
6. 删除 `TypedArray::class_finalize`（空函数），ClassDef 中设为 `None`

### Phase 1: ClassDef 扩展 ✅

**文件: `src/class/class_def.rs`**

新增 `ClassDef::new_with_atom()` 方法：
```rust
pub fn new_with_atom(
    name: &'static str,
    atom: Atom,
    proto_entries: &'static [FieldDef],
    ctor: Option<ClassCtorDef>,
) -> Self
```

### Phase 2: RuntimeImpl 扩展 ✅

**文件: `src/class/using.rs`**

新增 `RuntimeImpl::register_builtin_class(class_id, class_def)` 方法。

### Phase 3: `def_native_object!` 宏扩展 ✅

**文件: `src/class/native.rs`**

新增内部规则:
- `@builtin-struct` — 生成包装类型，无 thread_local CLASS_ID
- `@builtin-no-payload` — 无 payload 变体
- `@builtin-with-payload` — 有 payload 变体（支持 `init_fn:`）
- `@builtin-impl` — 生成 NativeObject impl（返回编译期常量 class_id）
- `@builtin_register` — 辅助创建 ClassDef（区分 init_fn 路径和 proto/ctor 路径）

新增公开入口:
- `builtin` + payload type + proto/ctor
- `builtin` + `init_fn:` + 可选 payload type
- `builtin` + `()` + proto/ctor（无 payload）

### Phase 4: Code Generator 更新 ✅

**文件: `build_stdclass.rs`**

1. `ClassInfo` 新增 `migrated: bool` 字段和 `migrated()`/`migrated_narrow()` 构造器
2. 生成 `register_all_builtin_classes()` 函数：
   - migrated 类: `CLASS_DEF_XXX::register(rt)?`
   - non-migrated 类: `rt.register_builtin_class(JS_CLASS_XXX, CLASS_DEF_XXX)?`
   - non-migrated 类: 内联 `wire_drop_lines` 设置 `payload_drop`
3. 删除旧的 `BUILTIN_CLASS_DEFS`、`BUILTIN_PAYLOAD_DROPS`、`wire_builtin_payload_drops()` 生成

### Phase 5: Builtin Class 迁移 ✅

**所有 `src/builtins/*.rs`** — 全部 73 个 builtin class 已迁移:

| 类别 | 类 | 特性 |
|------|-----|------|
| 核心 | Object, Array, Error, Number, Boolean | 无/有 payload |
| 核心 | String, Symbol, Date | payload + exotic(String) |
| 函数 | CFunction, BytecodeFunction, BoundFunction, CFunctionData | on_call, payload |
| 函数 | AsyncFunction, AsyncFunctionResolve/Reject | init_fn, on_call |
| 迭代器 | ForInIterator, ArrayIterator, StringIterator | 各种 payload |
| 迭代器 | Iterator, IteratorHelper/Wrap/Concat/Zip | init_fn |
| 迭代器 | AsyncFromSyncIterator, AsyncIterableIterator | init_fn |
| 生成器 | Generator, GeneratorFunction | init_fn |
| 生成器 | AsyncGenerator, AsyncGeneratorFunction | init_fn |
| 异步 | Promise, PromiseResolve/RejectFunction | on_call, payload |
| 集合 | Map, Set, WeakMap, WeakSet, MapIterator, SetIterator | payload |
| 正则 | RegExp, RegExpStringIterator | payload, init_fn |
| 代理 | Proxy | init_fn + exotic + on_call |
| 弱引用 | WeakRef, FinalizationRegistry | payload |
| ArrayBuffer | ArrayBuffer, SharedArrayBuffer | payload + on_finalize |
| TypedArray | 全部 12 个 + DataView | init_fn |
| 数值 | BigInt | payload (cfg-gated) |
| JSON | RawJSON | 无 payload |
| 模块 | ModuleNS | exotic |
| Intl | 全部 10 个 intl 类 | payload |

### Phase 5.5: NativeObject trait 扩展 ✅

**文件: `src/class/native.rs`**

1. 扩展 `NativeObject` trait:
   ```rust
   pub unsafe trait NativeObject: Clone + TryFrom<JSValue> + TryFrom<Object> {
       fn class_id() -> ClassID;
       fn class_name() -> &'static str;
       fn is_instance_of(obj: &Object) -> bool { ... }
       fn from_object(obj: &Object) -> Option<&Self> { ... }
       fn from_value(val: &JSValue) -> Option<Self> { ... }
   }
   ```
2. `Object::as_native_object<O>()` 和 `JSValue::as_native_object<O>()` 标记 `#[deprecated]`
3. `@impl` 和 `@builtin-impl` 宏规则生成 `class_name()` 返回 `CLASS_NAME` 常量
4. `TryFrom<JSValue>` 实现改用 `NativeObject::from_value()`

### Phase 6: 清理 ✅

**已完成** — 以下清理已执行：

1. **`src/class/class_def.rs`**: `ClassDef::builtin()` 和 `ClassDef::builtin_with_init_func()` 已删除
2. **`src/class/using.rs`**: `RuntimeImpl::register_builtin_classes()` 已删除，替换为 `build_stdclass.rs` 生成的独立函数 `register_all_builtin_classes()`
3. **`build_stdclass.rs`**: `wire_builtin_payload_drops()` 函数已删除，逻辑内联到 `gen_std_class_defs()` 生成的 `register_all_builtin_classes()` 中（`payload_drop_expr()` 保留，为非迁移类生成 `drop_payload_fn`）
4. **`src/class/mod.rs`**: 未使用的类型引用已清理

### Phase 7: PayloadPtr 简化 ✅

**commit 18a9ac60, 91e9e2a7**

1. 删除 `IntoPayloadPtr` trait（`src/object/payload_access.rs`）及其 build-time 生成的 impl（`build_stdclass.rs`）
2. 删除 `ScopedContext::create_instance_with_proto_class`、`create_instance_with_class`、`ordinary_create_from_constructor_class` 三个包装函数
3. 全部 ~60 个调用点改为直接调用 `PayloadPtr::new(class_id, payload)`
4. 清理 `ScopedContext` 实例创建 API：
   - `create_class_instance_with_payload` → `create_class_instance`
   - `create_class_instance` (无 payload) → `create_class_instance_no_payload`
   - `create_instance_using_proto` → `create_instance_with_proto`

## 3. 关键技术细节

### 3.1 宏的 builtin 分支 vs user 分支

| 方面 | Builtin | User |
|------|---------|------|
| class_id | 编译期常量 `(JS_CLASS_*, Atom::CONST_*)` | 运行时分配 |
| NativeObject::class_id() | 返回常量 | thread_local 读取 |
| register() | `rt.register_builtin_class(id, cd)` | `rt.register_class_def(cd)` |
| thread_local CLASS_ID | 不需要 | 需要 |
| init_fn 支持 | 支持 `init_fn:` 参数 | 不支持 |

### 3.2 payload 类型注意事项

宏内部对 payload 做 `Box::new()` 分配，因此：
- payload 参数**不能**传 `Box<T>`，否则变成 `Box<Box<T>>`
- 应传裸类型 `T`，如 `CollatorData` 而非 `Box<CollatorData>`

### 3.3 `register_builtin_class` vs `register_class_def`

```rust
register_builtin_class(class_id, class_def):
    // class_id 已预分配，直接使用
    // class_def.name_atom 已预分配
    register_class_internal(class_id, class_def)

register_class_def(class_def):
    // 分配新的 class_id
    class_def.name_atom = Atom::root_str(self, class_def.name)?;
    let class_id = self.user_class_id_serial;
    self.user_class_id_serial += 1;
    register_class_internal(class_id, class_def)
```

### 3.4 `init_fn:` 参数

用于 `ClassDef::builtin_with_init_func()` 类的迁移。宏内部展开为：
```rust
let mut cd = ClassDef::new_with($name, $init_fn);
cd.name_atom = $class_id.1;
```

### 3.5 Exotic methods 支持

```rust
def_native_object!(builtin, "Arguments", CLASS_DEF_ARGUMENTS,
    class_id: (JS_CLASS_ARGUMENTS, Atom::CONST_Arguments),
    ArrayPayload, &[], None,
    exotic: &js_arguments_exotic_methods  // 通过 builder 方法设入
);
```

## 4. test262 验证

| 测试集 | 结果 |
|--------|------|
| Boolean | 51/51 pass |
| Object + Error + Number | 3844/3844 pass |
| String + ArrayBuffer + SharedArrayBuffer | 1548/1548 pass |
| 单元测试 | 61/61 pass |
