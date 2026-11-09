---
name: zuqe-embedding
description: >
  REQUIRED when embedding the zuqe JavaScript engine in a Rust host, writing
  host/builtin native bindings, or developing zuqe extensions. Use when adding
  native functions, defining a native class with `def_native_object!`, creating
  a native module, implementing `GcTrace`/write barriers on payloads, running
  `zuqe-gc-audit`, `--gc-stress`, or reviewing GC safety. Triggers: zuqe,
  ScopedContext, CallContext, JSResult, CallbackFn, def_native_object,
  NativeObject, ClassDef, class_init, init_class_proto_extends,
  link_class_extends, create_module, FieldDef, GcTrace, GcBind, WriteBarrier,
  unsafe_empty_gc_trace, no-GC payload, bind_value, gc_audit, gc-stress,
  write barrier, zapi, native class, native module, payload. Excludes pure
  JS/TS application logic that does not touch the embedding API.
---

# zuqe 引擎嵌入开发 Skill

面向 **zuqe** 引擎嵌入与扩展开发的可复用方法、模板与检查流程。GC 规则与审计/WriteBarrier 要点已内嵌在本 skill 各主题中。

> API 可能演进，编写/评审时以源码与本 skill 的 checklist 校对。

## 读前须知：两条边界

1. **宿主/引擎内建（Rust，直接依赖 `zuqe`） vs zapi 第三方扩展（cdylib，ABI/FFI）是两条不同路径。**
   - 宿主/内建：使用 `ScopedContext`/`CallContext`/`JSResult`/`CallbackFn`、`def_native_object!`、`FieldDef`、`ScopedContext::create_module` 等 Rust API。
   - zapi 扩展：使用 `zuqe-zapi`/`ZqApi`、`ZqBodyFn`、`ZqRawValue`、`ZqFieldDef` 清单、`zq_module_init!` 等 ABI 接口；由宿主加载并经 zapi host 注册。**不要把两套类型/注册方式混用。**
2. **GC 正确性同时依赖 full mark 可达性与 generational write barrier。** `GcTrace` 覆盖 full mark；old→young 写入还必须 barrier（`gc_generations`）。缺一不可。不要给出 `gc_protect_transient` 之类错误修复建议。GC 部分先读 [GC安全守则](gc-safety.md#2-gc-rules唯一规则来源)，再按需进入写屏障/静态审计/动态 stress 细节。

## 主题索引

按需阅读对应主题。**任何嵌入开发都先走全局流程与 Checklist**；涉及 GC 节点时按 GC 目录先读守则，再进细节。

### 全局

| 主题 | 文件 | 何时读 |
|---|---|---|
| **全局工作流程与 Checklist** | [`workflow-checklist.md`](workflow-checklist.md) | 所有 zuqe 嵌入开发（function/class/module/GC/评审）的通用流程与检查项 |

### GC

| 主题 | 文件 | 何时读 |
|---|---|---|
| **GC安全守则（先读）** | [`gc-safety.md`](gc-safety.md#2-gc-rules唯一规则来源) | GC 规则（Rule 0-5）、两部分及缺失后果、最佳实践模板的唯一入口 |
| ↳ 细节：GC 追踪 | [`gc-trace.md`](gc-trace.md#2-实现示例) | `GcTrace`/`thread_local`/跨 JS 保活的实现示例、反例与排查 |
| ↳ 细节：写屏障与 GC 写安全 | [`gc-write-barrier.md`](gc-write-barrier.md#2-实现模式) | 屏障实现模式、失败模式、静态审计/动态 minor/gc stress 工具 |

### 绑定与扩展

| 主题 | 文件 | 何时读 |
|---|---|---|
| 绑定本地函数 | [`native-function.md`](native-function.md) | 注册宿主/内建原生函数、实现回调、处理参数/返回值/异常、`ScopedContext` 要点 |
| 编写 native class | [`native-class.md`](native-class.md) | `def_native_object!`、payload/`GcTrace`、无 payload/无 GC payload、构造/方法/原型/继承落地路径 |
| 定义 native module | [`native-module.md`](native-module.md) | `create_module`/`FieldDef` 导出、运行时模块加载集成、zapi 模块边界 |

## 关键依据（内嵌要点）

- **GC 规则**：唯一来源是 [GC安全守则](gc-safety.md#2-gc-rules唯一规则来源) §2 Rule 0-5（trace 与 barrier 缺一不可）。trace 实现示例/反例见 [`gc-trace.md`](gc-trace.md#2-实现示例)，屏障工具见 [`gc-write-barrier.md`](gc-write-barrier.md#3-检查流程)。本 skill 的其他文件不重复规则，只注明引用。
- **payload 形态**：零尺寸/无状态 payload 用 `def_native_object!(..., (), ...)`；无 GC 节点但字段不实现 `GcTrace` 用 `unsafe_empty_gc_trace!`；字段都可 trace 时用 `#[derive(GcTrace)]`。不要用空手写 impl 当默认，也不要用空 trace 宏掩盖 GC 字段（审计会检查）。
- **项目约束**：GC node safe；ZAPI 边界清晰；**类继承只有一条落地路径**（先建根类，再用 `ScopedContext::link_class_extends` / `init_class_proto_extends` 等原语链接；宿主自带类用 `def_native_object!(..., extends: X)`；禁止手写 `set_prototype` 建链；不开放 InitFn 给第三方）；按受众区分宿主/第三方范式。
- **代码模式参考**：`src/class/native.rs`（`def_native_object!`）、`src/class/using.rs`（`link_class_extends`/`init_class_proto_extends`/`use_class`）、`src/module/mdef.rs`、`src/function/*`、`src/gc/guard.rs`、`src/builtins/*`、`src/zapi/host.rs`、`examples/`、`tests/gc_audit.rs`。

## 使用方式

1. 先判定受众与路径：宿主/内建 Rust 绑定，还是 zapi 第三方 cdylib 扩展。混用是错误。
2. 先读全局 [工作流程与 Checklist](workflow-checklist.md)，按流程推进。
3. 读对应主题，复制模板并按真实类型/API 调整；不要凭记忆猜字段名。
4. **只要涉及 GC 节点，读 [GC安全守则](gc-safety.md#2-gc-rules唯一规则来源)**，按其中的 Rule 0-5 执行；trace 实现示例/反例见 [`gc-trace.md`](gc-trace.md#2-实现示例)，写屏障/静态审计/动态 minor/gc stress 见 [`gc-write-barrier.md`](gc-write-barrier.md#3-检查流程)。
5. 提交前跑静态审计（`cargo test --test gc_audit`）和定向动态验证（debug+`gc_generations`，必要时 `--gc-stress`），再过一遍 [workflow-checklist.md](workflow-checklist.md)。
