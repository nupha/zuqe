// ============================================================================
// aike (zhuque) 模块系统 panic 最小复现
//
// 现象：
//   对同一个 URL 连续两次动态 import：
//   第 1 次因缺失依赖("./missing-helper.ts" 不存在) 优雅地 reject；
//   第 2 次 import 同一 URL 时，aike 直接 panic：
//     thread 'main' panicked at src/module/mdef.rs:336:33:
//     called `Option::unwrap()` on a `None` value
//   进程崩溃退出（serve 场景下整个服务被拖垮）。
//
// 复现步骤：
//   1) 确保同目录下有 broken.ts（含缺失的相对依赖 ./missing-helper.ts）；
//   2) 用 aike 运行本脚本：  aike repro.mjs
//   3) 预期：attempt #1 打印"failed gracefully"，随后进程 panic 崩溃
//      （attempt #2 与 done 永不执行）。
//   4) 换 URL（如 ?fresh=N）验证对照：每次都优雅失败、不 panic。
//
// 必要条件：
//   1. 模块自身能编译，但至少一个依赖（相对导入）解析失败；
//   2. 失败后再次 import 同一个 URL（同 URL → 命中缓存复用，才会 panic；
//      换 URL（如 ?fresh=N）强制重新加载则每次都优雅失败）。
//
// 大致推测（根因）：
//   mdef.rs resolve_deps() 开头先置 self.resolved = true，再逐个解析依赖；
//   若某个依赖解析失败(如文件不存在)，函数返回 Err，但 resolved 已为 true，
//   且该失败依赖仍停留在 dependencies[] 的 module = None 状态。
//   js_free_modules(JS_FREE_MODULE_NOT_RESOLVED) 未能可靠地把这个半解析模块
//   从 loaded_modules 中清除，于是第 2 次 import 时 js_host_resolve_imported_module
//   在 loaded_modules 中命中该 def（resolved 已 true → resolve_deps 直接短路跳过），
//   随后 evaluate() → create_module_function() 遍历 dependencies 时
//   dep.module.as_mut().unwrap() 对 None 执行 unwrap → panic。
//
// 修复方向（zhuque）：
//   a) resolve_deps 失败时把 self.resolved 复位为 false（或在全部依赖成功后
//      再置位），保证重试会重新解析；
//   b) create_module_function 中不要直接 unwrap：dep.module 为 None 时改为
//      返回模块错误/重新 resolve，而不是 panic；
//   c) js_free_modules 确保把解析失败的模块从 loaded_modules 中彻底移除。
//
// 运行：aike repro.mjs
// ============================================================================

const url = "/tmp/panic-mini/broken.ts";

for (let i = 1; i <= 2; i++) {
  try {
    await import(url);
    console.log(`attempt #${i}: imported OK`);
  } catch (e) {
    console.log(`attempt #${i}: failed gracefully -> ${String(e?.message).slice(0, 60)}`);
  }
}
console.log("done"); // 永远走不到：第 2 次 import 已 panic
