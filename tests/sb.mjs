/*
 * 简化的字符串构建函数
 * 模拟 string_build2 的执行方式
 */

// 全局变量用于防止优化
var global_res;

// 简化的字符串构建函数
function simple_string_build(n, r) {
  var i, j;

  // 初始化参数 r 为空字符串
  r = "";

  // 循环构建字符串
  for (j = 0; j < n; j++) {
    // 每次循环添加字符到字符串
    for (i = 0; i < 10; i++) {
      r += "*";
    }
    // 将结果存储到全局变量
    global_res = r;
  }

  // 返回处理的操作数
  return n * 10;
}

// 基准测试函数
function benchmark_simple_string_build() {
  console.log("开始测试 simple_string_build 函数...");

  var iterations = [1, 5, 10];
  var result;

  for (var i = 0; i < iterations.length; i++) {
    var n = iterations[i];
    console.log(`\n测试 n = ${n}:`);

    // 调用函数，第一个参数是 n，第二个参数 r 不会使用（会被重置）
    result = simple_string_build(n, "unused_param");

    console.log(`返回值: ${result}`);
    console.log(`最终字符串长度: ${global_res.length}`);
    console.log(`最终字符串: "${global_res}"`);
    console.log(`global_res 长度: ${global_res.length}`);
  }
}

// 直接调用测试
if (import.meta.url === `file://${process.argv[1]}`) {
  benchmark_simple_string_build();
}

// 导出函数供其他模块使用
export { simple_string_build, benchmark_simple_string_build };
