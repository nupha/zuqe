# zuqe 嵌入开发工作流程与重点检测 Checklist（全局）

> 本文件是 **zuqe 嵌入开发的全局工作规范**，覆盖函数绑定、native class、native module、GC、
> 审计与评审；不是 GC 专属文档。GC 规则唯一来源是 [gc-safety.md](gc-safety.md#2-gc-rules唯一规则来源)；
> trace 实现示例/反例见 [gc-trace.md](gc-trace.md#2-实现示例)，写屏障/静态审计/动态 minor/gc stress 见
> [gc-write-barrier.md](gc-write-barrier.md#3-检查流程)。

## 标准流程

1. **需求澄清**
   - 要暴露什么能力（函数/class/module/事件/生命周期）？JS 侧 API 形状？同步/异步？错误语义？
   - 是否持有 GC 节点、是否跨 JS 调用、是否有长寿命缓存/thread-local？
2. **选型（边界先行）**
   - **宿主/引擎内建（Rust，直接依赖 `zuqe`）**：用 `ScopedContext`/`CallContext`/`JSResult`/`CallbackFn`、`def_native_object!`、`FieldDef`、`create_module`。适合嵌入式宿主、引擎内建能力。
   - **zapi 第三方 cdylib 扩展**：用 `zuqe-zapi`/`ZqApi`、`ZqBodyFn`、`ZqFieldDef`、`zq_module_init!` 等 ABI。适合可动态加载插件。
   - **禁止混用**两套范式/类型/注册方式。选型结论写入实现说明或 PR 描述。
3. **实现**
   - 函数绑定：[native-function.md](native-function.md)
   - native class：[native-class.md](native-class.md)
   - native module：[native-module.md](native-module.md)
   - 设计时先想 GC：payload 字段、thread-local、跨 JS 持有、后写路径。
4. **GC 自查**（先读 [gc-safety.md](gc-safety.md#2-gc-rules唯一规则来源)；trace 看 [gc-trace.md](gc-trace.md#2-实现示例)，屏障看 [gc-write-barrier.md](gc-write-barrier.md#2-实现模式)）
   - 补 `GcTrace`（无 GC 节点：零尺寸 payload 用 `()`，非零尺寸用 `unsafe_empty_gc_trace!`）、
     处理 Rule 4 跨 JS；加 guard slot/setter、确认 owner/无 whole-field replacement。
5. **静态 GC 审计**（具体做法见 [gc-write-barrier.md](gc-write-barrier.md#4-静态审计zuqe-gc-auditshape)）
   - 标准布局：[`tests/gc_audit.rs` + `gc_audit_exemption.txt`](gc-write-barrier.md#42-标准布局crate-内)（`zuqe::gc_audit_test!(min_roots)`）。
   - `cargo test --test gc_audit`（目标 crate）；必要时 `zuqe-gc-audit <src>` 看报告；检查项见 [静态审计](gc-write-barrier.md#41-它检查什么)。
   - 新增 exemption 必须短、诚实、分类合理；修 stale；规范见 [gc-write-barrier.md](gc-write-barrier.md#45-exemption-规范)。
6. **GC 运行时验证（定向）**（具体做法见 [gc-write-barrier.md](gc-write-barrier.md#5-动态-minor-验证timing)）
   - debug + `gc_generations` 下 promote owner → 写入 → `gc_minor_now()`，缺 barrier 应在 `dbg_assert_no_stale_old_edges` 报 `missing write barrier`。
   - 定向 case 用 `--gc-stress`（或 `gc_set_stress(true)`，小脚本）复现一次；见 [gc-write-barrier.md](gc-write-barrier.md#6-gc-stress)。
   - 补/跑与改动相关的单测、集成测试、示例。
7. **评审**
   - 重点看：边界是否混用、继承建链是否合规、GC 形态+时序风险、异常/`this` 校验、exemption 理由；逐项过 [Checklist](workflow-checklist.md#重点检测-checklist)。

## 重点检测 Checklist

### A. 路径与边界

- [ ] 已明确受众：宿主/内建 Rust **或** zapi 第三方 cdylib；未混用 `ScopedContext`/`FieldDef`/`def_native_object!` 与 `Zq*` ABI 类型。
- [ ] native module/函数/class 的注册方式与选型一致（`create_module`/`FieldDef` vs zapi 清单/host 加载）。
- [ ] 未把内建特权机制（如 InitFn、pub(crate) 原语）当作第三方可用 API。

### B. 本地函数绑定

- [ ] 回调签名 `fn(&mut ScopedContext, CallContext) -> JSResult`；`length`/`magic` 正确。
- [ ] 参数读取做边界/类型校验；失败时 `throw_*` 并 `Err(-1)`，不返回伪成功。
- [ ] `this` 按需校验（方法尤其是 native class 方法）。
- [ ] 属性/导出可见性标志符合语义。

### C. Native class 与继承

- [ ] 使用 `def_native_object!` 标准形态；`#[repr(transparent)]` wrapper 不手工实现 `NativeObject`。
- [ ] payload 访问通过生成的安全方法；class_id 校验到位。
- [ ] 零尺寸/无状态 payload 用 `()`；无 GC 节点的非零尺寸 payload 用 `unsafe_empty_gc_trace!`，
  没有空手写 `impl GcTrace`。宏类型的字段不能含 GC 节点（审计会检查）。
- [ ] context 中使用前 `use_class`/初始化路径正确。
- [ ] **继承合规（只有一条落地路径）**：
  - [ ] 宿主类用 `def_native_object!(..., extends: Parent::class_id)`（按引擎支持形态）；
  - [ ] 未手写 `set_prototype` 建 class prototype/constructor 继承链；
  - [ ] 内建类复用 `link_class_extends`/`init_class_proto_extends`/`bind_ctor_proto*` 等原语，不另造建链逻辑；
  - [ ] 未开放 InitFn 给第三方。
- [ ] `extends:` 子类有独立 payload/`GcTrace`（不共享父类 payload 类型）。

### D. Native module

- [ ] `create_module(name, &'static [FieldDef])`；导出表 `'static`；未用已弃用 `register_module`（新代码）。
- [ ] loader/normalizer 为 plain `fn` 指针语义，宿主状态走 `user_state`/明确机制。
- [ ] 模块名/解析策略与宿主集成一致。

### E. GC 规则检查（引用）

> 所有 GC 规则以 [gc-safety.md](gc-safety.md#2-gc-rules唯一规则来源) §2 Rule 0-5 为准；本文件不重复规则。
> trace 实现示例/反例见 [gc-trace.md](gc-trace.md#3-反例集)；屏障工具见 [gc-write-barrier.md](gc-write-barrier.md#3-检查流程)。

- [ ] 逐条对照 `gc-safety.md` Rule 0-5 完成自查；示例/反例见 `gc-trace.md` / `gc-write-barrier.md`。
- [ ] trace 侧：按 `gc-trace.md` 的示例/反例检查；无 GC 节点的 payload 用 `()` 或
  `unsafe_empty_gc_trace!`，宏类型不含 GC 字段。
- [ ] 屏障侧：按 `gc-write-barrier.md` 的静态审计/动态 minor/gc stress 流程执行。
- [ ] 评审时未出现 `gc-trace.md` / `gc-write-barrier.md` 列出的反例，也未建议其修复方式。

### F. GC 审计与验证（工具流程）

- [ ] `tests/gc_audit.rs` + `gc_audit_exemption.txt`（若需）按 [标准布局](gc-write-barrier.md#42-标准布局crate-内)；`zuqe::gc_audit_test!(min_roots)` 合理。
- [ ] `cargo test --test gc_audit` 通过；无 stale exemption；`unsafe_empty_gc_trace!` 类型不含 GC 节点；
  检查项见 [静态审计](gc-write-barrier.md#41-它检查什么)。
- [ ] exemption 分类/理由符合 [规范](gc-write-barrier.md#45-exemption-规范)。
- [ ] 新 old→young 路径已在 debug+`gc_generations` 下[定向验证](gc-write-barrier.md#5-动态-minor-验证timing)；定向 case 跑过 [gc stress](gc-write-barrier.md#6-gc-stress)（可行时）。
- [ ] 理解局限：审计/validator 非时序完备证明；release 正确性靠 guard+setter+review；见 [工具局限](gc-write-barrier.md#7-工具局限)。

## 交付物自检（skill/实现任务通用）

- [ ] 覆盖函数绑定、native class、native module、GC trace、WriteBarrier、静态审计、动态 stress、流程+checklist。
- [ ] 模板可操作，API/宏名称与当前 zuqe 模式一致（不确定处已引用源码路径）。
- [ ] 关键源码位置可引用；GC 规则与主题内容完整；区分宿主/内建与 zapi 第三方约束。
- [ ] GC 语义准确，无错误建议。
