# 定义 native module

> 区分两条路径：**宿主/引擎内建 Rust native module**（`ScopedContext::create_module` + `FieldDef`）与 **zapi 第三方 cdylib module**（`ZqFieldDef` 清单 + `zq_module_init!`/host 加载）。不要混用 API 与类型。

## 宿主/内建：`ScopedContext::create_module`

源码：`src/module/mdef.rs`。`create_module` 创建 native module，声明导出名，并在 init 时按 `FieldDef` 设置导出值。

```rust
use zuqe::{CallContext, FieldDef, JSResult, JSValue, ScopedContext, JS_PROP_C_W_E};

fn js_add(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let a = cc.arg(0).to_int32(ctx)?;
    let b = cc.arg(1).to_int32(ctx)?;
    Ok(JSValue::from(a + b))
}

// 必须是 'static：create_module 接收 &'static [FieldDef]
pub static MY_MOD_EXPORTS: &[FieldDef] = &[
    FieldDef::FUNC("add", 2, js_add, 0),
    // FieldDef::STR / INT32 / FLOAT64 / BOOL / UNDEFINED / LAZY* 等按 fields.rs 支持情况使用
];

pub fn install_my_module(ctx: &mut ScopedContext) -> Result<(), i32> {
    ctx.create_module("my:native", MY_MOD_EXPORTS)?;
    Ok(())
}
```

要点：
- **导出表生命周期**：`entries: &'static [FieldDef]`，通常用 `static`/`const` + 字面量/常量字段。
- **导出名**：由 `FieldDef.name` 声明；init 回调会 `set_export_values`。
- **模块名**：字符串标识；与宿主的模块解析/加载策略配合（见下文）。native module 由引擎记录在 loaded modules 中。
- `register_module` 已弃用，使用 `create_module`。

## 运行时/上下文集成点（宿主侧）

`RuntimeImpl` 暴露模块解析与加载钩子（`src/module/mdef.rs`）：

- `set_module_normalizer(norm_func: ModuleNormalizeFn)`：自定义 specifier 归一化（替换内建相对/绝对路径归一化策略）。签名 `fn(ctx, base_name: &Url, name: &str) -> Result<Url, i32>`。
- `set_module_loader(load_func: ModuleLoadFn)`：自定义模块加载；默认 loader 使 import 抛错。签名 `fn(ctx, module_name: &Url, attrs: JSValue) -> Result<ModuleDef, i32>`。

这些回调是 **plain `fn` 指针**，不携带宿主状态；宿主状态应放在 `RuntimeImpl::user_state`，回调通过 `ctx.rt()` 访问。设置后由引擎在解析/加载依赖时调用。

典型宿主集成流程（示意）：
1. `Runtime::create()` / `create_context`。
2. 注册/创建需要的 native modules（`create_module`），或确保 loader 能返回它们。
3. 设置 `set_module_normalizer`/`set_module_loader`（若需要文件系统、虚拟 FS、别名、`node:`/`aike:` 等策略）。
4. 执行入口模块/脚本。

> 具体 loader 如何定位 native module vs 编译 JS/bytecode module 取决于宿主设计；保持策略单一、可测试。

## GC / 作用域注意事项

> 规则见 [`gc-safety.md`](gc-safety.md#2-gc-rules唯一规则来源) §2 Rule 0-5；trace 示例见
> [`gc-trace.md`](gc-trace.md#2-实现示例)；屏障实现与工具见 [`gc-write-barrier.md`](gc-write-barrier.md#2-实现模式)。

module init/导出值、模块级记录和 loader 状态按 Rule 1/2/4/5 对照；本文件不重复规则。

## zapi 第三方 native module（边界）

zapi 扩展以 **cdylib** 形式分发，通过 C ABI 与引擎交互：

- 导出清单使用 `ZqFieldDef`（`FUNC`/`CLASS`/`GETSET`/常量等），示例见 `examples/zapi_raw`、`examples/zapi_basic`、`examples/zapi_event` 等。
- 扩展入口通过 `zq_module_init!`（或等价导出）暴露模块信息；宿主加载走 `src/zapi/host.rs`（`load_ext_lib`、解析符号、注册 class/exports、module finalizer 等）。
- 回调签名为 `ZqBodyFn`（`ZqRawCtx` + `ZqCallInfoS`），返回 `ZqRawValue`；异常用 `ZQ_CONST_VALUE_SLOT_EXCEPTION`/API 约定。
- zapi 路径有独立的 GC/生命周期约束：class `gc_trace` 回调必须报告其持有的全部 GC 字段；引擎对 **old zapi 实例**在每次 minor 做 payload 重扫作为无 barrier ABI 的补偿（仍依赖 `gc_trace` 报告的边）。extension lib 由 host 进程级缓存并保持映射（不卸载），以避免导出指针悬垂。

**选型规则**：写宿主内置能力或引擎内建模块 → Rust `create_module`/`FieldDef`。写可被动态加载的第三方插件 → zapi ABI，并遵循 zapi 文档/示例/宿主约束。**不要**在 zapi 扩展里假装拥有宿主 Rust API，也不要把 zapi 原始值/注册方式塞进宿主内建代码。

## 反模式

- 使用已弃用 `register_module` 而非 `create_module`（新代码）。
- 把 `FieldDef` 表做成非 `'static` 临时数据传给 `create_module`。
- 在 loader/normalizer 中存隐式全局可变状态却不通过 `user_state`/明确同步管理（`fn` 指针无捕获）。
- 混淆 native module（Rust 注册）与 JS module（源码/bytecode，由 loader/编译管线提供）。
