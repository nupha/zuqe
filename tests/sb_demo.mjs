/*
 * 详细演示 string_build2 执行方式的示例
 * 展示参数传递和函数调用的完整过程
 */

// 全局变量
var global_res;

// 完整模拟 string_build2 的函数
function demo_string_build2(n, r) {
  console.log(`函数开始执行:`);
  console.log(`  - 传入的 n 值: ${n}`);
  console.log(`  - 传入的 r 值: ${r} (类型: ${typeof r})`);

  var i, j;

  // 关键：立即重置 r 参数
  r = "";
  console.log(`  - 初始化后 r 值: "${r}"`);

  console.log(`开始循环构建字符串:`);

  // 外层循环
  for (j = 0; j < n; j++) {
    console.log(`  外层循环第 ${j + 1} 次: r.length = ${r.length}`);

    // 内层循环添加字符
    for (i = 0; i < 10; i++) {
      r += "*";
    }

    // 更新全局变量
    global_res = r;
    console.log(`    内层循环后: r = "${r}" (长度: ${r.length})`);
  }

  console.log(`函数执行完成:`);
  console.log(`  - 最终 r 值: "${r}" (长度: ${r.length})`);
  console.log(`  - 返回值: ${n * 10}`);

  return n * 10;
}

// 模拟基准测试的调用方式
function simulate_benchmark_call(test_name, test_function) {
  console.log(`\n=== ${test_name} ===`);

  // 模拟 bench 函数中的调用方式：只传递一个参数 n
  var n = 3; // 测试用的迭代次数

  console.log(`基准测试框架调用: ${test_function.name}(${n})`);
  console.log(`注意：只传递了一个参数，第二个参数 r 将为 undefined`);

  var result = test_function(n); // 只传递一个参数！

  console.log(`测试结果: ${result}`);
  console.log(`global_res: "${global_res}" (长度: ${global_res.length})`);
}

// 对比不同的调用方式
function compare_call_methods() {
  console.log("=== 对比不同的调用方式 ===\n");

  // 方式 1：模拟基准测试的调用（只传一个参数）
  console.log("方式 1: 基准测试方式调用（只传一个参数）");
  console.log("demo_string_build2(3) - 第二个参数 undefined");
  var result1 = demo_string_build2(3);
  console.log(`结果: ${result1}\n`);

  // 方式 2：传入两个参数（但第二个参数会被忽略）
  console.log("方式 2: 传入两个参数（第二个参数会被重置）");
  console.log('demo_string_build2(3, "ignored_value") - 第二个参数被重置');
  global_res = ""; // 重置
  var result2 = demo_string_build2(3, "ignored_value");
  console.log(`结果: ${result2}\n`);

  // 方式 3：传入非字符串值
  console.log("方式 3: 传入非字符串值");
  console.log("demo_string_build2(3, 12345) - 非字符串值被重置");
  global_res = ""; // 重置
  var result3 = demo_string_build2(3, 12345);
  console.log(`结果: ${result3}\n`);
}

// 主函数
function main() {
  console.log("string_build2 执行方式详细演示\n");
  console.log("这个函数展示了基准测试中 string_build2 的完整执行流程\n");

  compare_call_methods();

  console.log("=== 总结 ===");
  console.log("1. string_build2 函数接收两个参数：n 和 r");
  console.log("2. 基准测试只传递第一个参数 n，r 默认为 undefined");
  console.log("3. 函数内部立即将 r 重置为空字符串");
  console.log("4. 无论传入什么值，r 都会被重置为 ''");
  console.log("5. 函数执行字符串构建操作，最终返回 n * 内层循环次数");
}

// 运行演示
if (import.meta.url === `file://${process.argv[1]}`) {
  main();
}

export { demo_string_build2, simulate_benchmark_call, compare_call_methods };
