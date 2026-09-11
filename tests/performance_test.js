// 性能测试脚本 - 用于分析时间差异
console.log("开始性能测试...");

// 测试1: 基础循环性能
function test_basic_loop() {
  console.log("测试1: 基础循环");
  let start = Date.now();

  let sum = 0;
  for (let i = 0; i < 100000; i++) {
    sum += i;
  }

  let end = Date.now();
  console.log(`基础循环耗时: ${end - start}ms, 结果: ${sum}`);
  return end - start;
}

// 测试2: 字符串操作性能（类似debug.js但规模更小）
function test_string_operations() {
  console.log("测试2: 字符串操作");
  let start = Date.now();

  for (let j = 0; j < 1000; j++) {
    let str = String.fromCharCode(j % 256);
    let res = str.replace(/\S+/g, "test");
  }

  let end = Date.now();
  console.log(`字符串操作耗时: ${end - start}ms`);
  return end - start;
}

// 测试3: 对象创建和垃圾回收压力
function test_gc_pressure() {
  console.log("测试3: GC压力测试");
  let start = Date.now();

  for (let i = 0; i < 10000; i++) {
    let obj = {
      id: i,
      data: new Array(10).fill(i),
      text: "test" + i,
    };
    // 让对象立即变为垃圾
  }

  let end = Date.now();
  console.log(`GC压力测试耗时: ${end - start}ms`);
  return end - start;
}

// 运行多次测试来观察时间差异
console.log("=== 开始多轮测试 ===");

let results = {
  basic_loop: [],
  string_ops: [],
  gc_pressure: [],
};

for (let round = 1; round <= 5; round++) {
  console.log(`\n--- 第 ${round} 轮测试 ---`);

  results.basic_loop.push(test_basic_loop());
  results.string_ops.push(test_string_operations());
  results.gc_pressure.push(test_gc_pressure());
}

// 计算统计信息
function calculate_stats(arr) {
  let min = Math.min(...arr);
  let max = Math.max(...arr);
  let avg = arr.reduce((a, b) => a + b, 0) / arr.length;
  let variance = max - min;

  return { min, max, avg: avg.toFixed(2), variance };
}

console.log("\n=== 测试结果统计 ===");
console.log("基础循环:", calculate_stats(results.basic_loop));
console.log("字符串操作:", calculate_stats(results.string_ops));
console.log("GC压力测试:", calculate_stats(results.gc_pressure));

console.log("\n性能测试完成！");
