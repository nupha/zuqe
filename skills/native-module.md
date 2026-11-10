# 模块：ESM 加载与 native module

> 两条不同的“模块”路径：
>
> 1. **JS/ESM 模块**：源码/字节码由宿主的 loader 提供，引擎负责解析、链接、
>    求值。默认 loader 对每次 import 抛错，必须由宿主设置。
> 2. **宿主/内建 native module**：Rust 通过 `ScopedContext::create_module` +
>    `FieldDef` 注册，导出值为 Rust 函数/常量/类。
>
> 第三方可动态加载的 cdylib 扩展是第三条路径（zapi ABI），见
> [zapi-extension.md](zapi-extension.md)。不要混用 `FieldDef` 与 `ZqFieldDef`。

模块系统内部实现（ModuleDef、链接/求值、循环依赖）见 `docs/module.md`。

## 1. ESM 模块支持

### 1.1 默认行为

每个 Runtime 启动时都装有：

- **默认 normalizer**：绝对路径 → URL；相对说明符按导入模块的 URL join。
  `file://` URL 在归一化后统一 canonicalize，symlink 别名会折叠成同一个缓存 key。
- **默认 loader**：stub，每次 import 抛
  `ReferenceError: could not load module '<name>'`。

因此引擎能编译，但**不设置 loader 时任何 import 都失败**。normalizer 只在需要
改变解析策略时才覆盖。

### 1.2 回调签名

```rust
use url::Url;
use zuqe::{JSValue, ModuleDef, ScopedContext};

pub type ModuleNormalizeFn =
    fn(ctx: &mut ScopedContext, base_name: &Url, name: &str) -> Result<Url, i32>;

pub type ModuleLoadFn =
    fn(ctx: &mut ScopedContext, module_name: &Url, attrs: JSValue) -> Result<ModuleDef, i32>;
```

- `attrs` 是 import attributes（例如
  `import data from './config.json' with { type: 'json' }` 里的 `{ type: "json" }`）。
- 两个回调都是普通 `fn` 指针，**不携带宿主状态**；状态放
  `RuntimeImpl::user_state`，回调里用 `ctx.rt()` 访问。
- 设置在 `Runtime` 上（`Runtime` deref 到 `RuntimeImpl`）：

```rust
rt.set_module_normalizer(module_normalize); // 可选
rt.set_module_loader(module_loader);       // 不设置则 import 全失败
```

### 1.3 文件系统 loader 示例

```rust
use url::Url;
use zuqe::{JSValue, ModuleDef, ScopedContext, url_to_abspath};

fn module_normalize(
    _ctx: &mut ScopedContext,
    base_name: &Url,
    name: &str,
) -> Result<Url, i32> {
    base_name.join(name).map_err(|_| -1)
}

fn module_loader(
    ctx: &mut ScopedContext,
    module_name: &Url,
    attrs: JSValue,
) -> Result<ModuleDef, i32> {
    let path = match zuqe::url_to_abspath(module_name) {
        Some(p) => p,
        None => {
            ctx.throw_reference_error(&format!(
                "could not load module '{module_name}': not a file:// URL"
            ));
            return Err(-1);
        }
    };

    let is_json = attrs.is_object()
        && attrs
            .get_property_str(ctx, "type")
            .map(|v| v.as_string(true).is_some_and(|s| {
                s.content().encode_utf8(false).as_ref() == "json"
            }))
            .unwrap_or(false);

    match std::fs::read_to_string(&path) {
        Ok(content) => {
            if is_json {
                // JSON 模块包成 `export default <json>`
                let src = format!("export default {content}\n");
                ctx.compile_module(src.as_bytes(), module_name, false)
            } else {
                ctx.compile_module(content.as_bytes(), module_name, false)
            }
        }
        Err(_) => {
            ctx.throw_reference_error(&format!("could not load module '{module_name}'"));
            Err(-1)
        }
    }
}
```

`compile_module(source, module_url, strip)` 编译并返回 `ModuleDef`；`strip`
对应 `EvalFlags::STRIP`。

### 1.4 `import.meta`

模块内可用：

- `import.meta.url`：模块 URL（`module_name`）。
- `import.meta.main`：是否是主模块。

模块入口必须用 `EvalName::BaseUrl(path_to_url(filename)?)`；用
`EvalName::Name` 会得到错误（module eval requires an URL base）。

### 1.5 已加载模块查询

`ScopedContext::loaded_module_names(filter)` 可列出模块名：

```rust
use zuqe::ModuleNameFilter;
let native = ctx.loaded_module_names(ModuleNameFilter::NativeOnly);
let bytecode = ctx.loaded_module_names(ModuleNameFilter::BytecodeOnly);
let all = ctx.loaded_module_names(ModuleNameFilter::All);
```

## 2. 宿主/内建 native module

### 2.1 `ScopedContext::create_module`

源码：`src/module/mdef.rs`。

```rust
use zuqe::{CallContext, FieldDef, JSResult, JSValue, ScopedContext, JS_PROP_C_W_E};

fn js_add(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let a = cc.arg(0).to_number(ctx)?.to_i32();
    let b = cc.arg(1).to_number(ctx)?.to_i32();
    Ok(JSValue::from(a + b))
}

// 必须是 'static：create_module 接收 &'static [FieldDef]
pub static MY_MOD_EXPORTS: &[FieldDef] = &[
    FieldDef::FUNC("add", 2, js_add, 0),
    FieldDef::INT32("ANSWER", 42, JS_PROP_C_W_E),
    // STR / FLOAT64 / BOOL / UNDEFINED / LAZY_VALUE / LAZY_OBJECT / CLASS /
    // GETSET / GETSET_ENUM / ALIAS 等见 src/function/fields.rs
];

pub fn install_my_module(ctx: &mut ScopedContext) -> Result<(), i32> {
    ctx.create_module("my:native", MY_MOD_EXPORTS)?;
    Ok(())
}
```

要点：

- `entries: &'static [FieldDef]`；用 `static`/`const` + 字面量/常量字段。
- 导出名由 `FieldDef.name` 声明；init 时 `set_export_values` 设置。
- 模块名是字符串标识，与 loader/normalizer 策略配合；native module 记录在
  loaded modules 中。
- `register_module` 已 deprecated，新代码用 `create_module`。
- `create_module` 返回 `ModuleDef`，可用于检查/手动求值。

### 2.2 FieldDef 常用构造器

| 构造器 | 导出 |
|---|---|
| `FieldDef::FUNC(name, length, cb, magic)` | 原生函数 |
| `FieldDef::FUNC_ENUM(...)` | 可枚举函数 |
| `FieldDef::GETSET(name, get, set, magic)` | getter/setter（non-enumerable） |
| `FieldDef::GETSET_ENUM(...)` | 可枚举 getter/setter（WebIDL 宿主对象） |
| `FieldDef::STR(name, "v", flags)` | 字符串常量 |
| `FieldDef::INT32/FLOAT64/BOOL/UNDEFINED` | 基本常量 |
| `FieldDef::LAZY_VALUE(name, fn, flags)` | 模块求值时惰性构造 JS 值 |
| `FieldDef::OBJECT(name, entries, flags)` | 嵌套对象 |
| `FieldDef::CLASS(name, || Type::class_id(), flags)` | 类构造器 |
| `FieldDef::ALIAS/ALIAS_BASE` | 别名 |

### 2.3 运行时集成流程

1. `Runtime::create()` / `create_context`。
2. 在 setup 或之后调用 `create_module("name", ENTRIES)` 注册 native modules。
3. 如需文件系统/虚拟 FS/别名，设置 `set_module_normalizer` / `set_module_loader`。
4. 执行入口脚本/模块（`EvalName::BaseUrl`）。

> loader 如何区分 native module、编译 JS 模块和 zapi 扩展由宿主设计决定；保持
> 策略单一、可测试。zjs 的 `js_module_loader` 是一个完整例子。

## 3. zapi 模块边界

zapi 扩展以 **cdylib** 分发：

- 导出清单用 `ZqFieldDef`（`FUNC`/`CLASS`/`GETSET`/常量等），入口由
  `#[zq_module(exports = [...])]` 生成 `zq_module_register`。
- 宿主需开启 `zapi` feature，并用 loader 调用
  `ScopedContext::load_zapi_extension(path)`；zjs 的 loader 已按扩展名识别
  `.so`/`.dylib`/`.dll`。
- 回调签名是 `ZqBodyFn`，类型是 `ZqRawCtx`/`ZqCallInfo`/`ZqRawValue`；异常用
  `ctx.throw_*` 返回 `ZqError`。
- zapi class 的 `gc_trace` 必须报告其持有的全部 GC 字段；引擎对 old zapi 实例
  每次 minor 重扫 payload，作为没有 barrier ABI 的补偿。
- extension lib 由宿主进程级缓存并保持映射，不卸载。

写 zapi 扩展见 [zapi-extension.md](zapi-extension.md)。

## 4. GC / 作用域注意事项

> 规则唯一来源：[gc-safety.md](gc-safety.md#2-gc-rules唯一规则来源) §2 Rule 0-5。

模块记录（capability/resolver/namespace/async parent 等）是长寿命 old 容器，
宿主写入按 Rule 5 走 setter/guard；模块级缓存的 `JSValue`/`Object` 必须被
`GcTrace` 覆盖；跨 JS re-entry 的临时值按 Rule 4 保活。

## 5. 反模式

- 用已弃用 `register_module` 而非 `create_module`。
- 把非 `'static` 的 `FieldDef` 表传给 `create_module`。
- 在 normalizer/loader 里存隐式全局可变状态（`fn` 指针没有捕获）。
- 混淆 native module（Rust 注册）与 JS module（loader 编译）。
- 在 zapi 扩展里使用宿主 Rust API，或把 `Zq*` ABI 类型塞进宿主内建代码。
- 模块入口用 `EvalName::Name` 而不是 `BaseUrl`。
