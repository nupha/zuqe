# Tiered JIT 使用

> 面向宿主配置。JIT 的验证、lowering、代码缓存、测试矩阵等内部细节见
> `docs/jit.md`。这里只讲公开 API 与宿主需要知道的行为。

## 1. 开关与语义

```toml
[dependencies]
zuqe = { version = "0.11", features = ["jit"] } # jit 在默认 feature 中
```

- `jit` 编译**已经解析好的 bytecode**，不碰 parser/emitter/bytecode 格式。
- 默认 `JitTierPolicy::Automatic` 下，结果与解释器 bit-for-bit 一致；后端无法
  翻译的 opcode 会让函数保持解释执行（fail-closed）。
- 不是 tracing/GC-integrated JIT：值仍在解释器的 frame buffer 中，没有
  register-resident GC root。
- `with`/generator/`async`/`eval`/`import` 等 opcode 按设计拒绝编译。

## 2. 基本用法

```rust
use zuqe::{Jit, JitConfig, JitTierPolicy};

let mut rt = zuqe::Runtime::create()?;
let mut ctx = rt.create_context(zuqe::ScopedContext::ECMA_CONTEXT_SETUP)?;

let jit = Jit::attach_to(
    &rt,
    JitConfig::builder()
        .tier_policy(JitTierPolicy::Automatic)
        .call_threshold(32)
        .loop_threshold(56)
        .tier2_call_threshold(1024)
        .tier2_loop_threshold(2048)
        .build(),
)?;

ctx.eval(
    b"for (var i = 0, s = 0; i < 1e6; i++) s += i;",
    &zuqe::EvalName::Name("app.js".into()),
    zuqe::EvalType::Global,
    zuqe::EvalFlags::empty(),
)?;

println!("{:?}", jit.metrics());
```

`Jit::attach_to(&runtime, config)`：

- 每个 `Runtime` 一个 JIT；没有 Cranelift 后端时返回 `Err(JitError)`。
- `Jit` 持有 `Runtime` 的强引用，保证 handle 存活期间 runtime 不被释放；不必手动
  控制 drop 顺序，但同一个 runtime 只能 attach 一次。

## 3. 编译时机

**默认自动编译，宿主不需要每轮调用 `poll()`。** 引擎在三个安全点轮询：

1. 解释器 trampoline 的 frame 边界；
2. 事件循环 tick；
3. 已执行的 loop back edge（同时支持 back-edge OSR：正在跑的循环可以切到
   已安装的编译代码）。

验证与 lowering 在 worker 线程执行；runtime 线程只 dispatch/drain/install。
因此：

- 单个顶层热循环即使不跨 frame 边界，也会在自己的 back edge 排队并进入编译。
- `Jit::poll()` 仅在需要精确控制时使用；配合
  `JitConfig::builder().auto_poll(false)` 可完全手动驱动。

## 4. Tier 与阈值

| 策略 | 行为 |
|---|---|
| `Interpreter` | 不安装任何 native 代码，hook 惰性 |
| `Tier1` | 只编译 baseline tier |
| `Tier2` | 总是对已记录 feedback 的部分做特化 |
| `Automatic` | 默认；两个 gate：第一次编译 baseline，第二次 promote 到优化 tier |

每个 gate 是 entries 与 back edges 的加权预算：
`L×calls + C×loops ≥ C×L`。`call_threshold`/`loop_threshold` 对应第一 gate，
`tier2_call_threshold`/`tier2_loop_threshold` 对应第二 gate。

## 5. 指标

`jit.metrics()` 返回 `JitMetrics`，包含：

- function entries、back edges、queue length/drops；
- verified/rejected/compiled counts、native entries；
- interrupts、compile failures/timeouts；
- speculative fast-path counts、code bytes；
- tiering 计数：`tier2_promotions`、`functions_recompiled`、`tier_downs`。

## 6. 宿主注意

- JIT 失败的函数继续解释执行，不需要宿主回退。
- 不要依赖 JIT 的编译时刻做语义保证；正确性以解释器为参考。
- Apple Silicon 的 `MAP_JIT` 尚未实现；当前支持 Linux x86_64/aarch64（qemu）、
  Intel macOS、Windows x86_64。
- 生产环境不要开 `jit_dump`（会写出 `.clif`/`.bin`，且路径是编译期 feature）。
