// 1. 基本的 Async Generator
async function* basicAsyncGenerator() {
  console.log("1. 开始生成数据");
  yield 1;
  console.log("2. 生成第二个值");
  yield 2;
  console.log("3. 生成第三个值");
  yield 3;
  console.log("4. 结束");
}

// 2. Async Generator 与 Promise 结合
async function* asyncGeneratorWithPromises() {
  console.log("Async Generator: 等待 Promise...");
  const result = await new Promise((resolve) =>
    setTimeout(() => resolve("延迟数据"), 100),
  );
  yield result;
  yield "直接数据";
}

// 3. 抛出错误处理
async function* generatorWithThrow() {
  try {
    yield "正常值";
    yield "第二个正常值";
  } catch (error) {
    console.log("捕获到错误:", error.message);
    yield "恢复后的值";
  }
}

// 4. 使用 return 提前结束
async function* generatorWithReturn() {
  yield "第一个";
  yield "第二个";
  yield "第三个";
}

// 5. Async Generator 的 for-await-of 遍历
async function* countGenerator() {
  for (let i = 1; i <= 5; i++) {
    await new Promise((r) => setTimeout(r, 50));
    yield i;
  }
}

// 6. 嵌套 Async Generator
async function* nestedGenerator() {
  yield* [1, 2, 3];
  yield* ["a", "b", "c"];
}

// // 7. 异步迭代器协议
// const asyncIterable = {
//   async *[Symbol.asyncIterator]() {
//     yield "第一项";
//     await new Promise((r) => setTimeout(r, 50));
//     yield "第二项";
//     yield "第三项";
//   },
// };

// 测试运行函数
async function runTests() {
  console.log("=".repeat(50));
  console.log("Async Generator 测试开始");
  console.log("=".repeat(50));

  // 测试 1: 基本 Async Generator
  console.log("\n--- 测试 1: 基本 Async Generator ---");
  const gen1 = basicAsyncGenerator();
  console.log(await gen1.next());
  console.log(await gen1.next());
  console.log(await gen1.next());
  console.log(await gen1.next());

  //   // 测试 2: Async Generator 与 Promise
  //   console.log("\n--- 测试 2: Async Generator 与 Promise ---");
  //   const gen2 = asyncGeneratorWithPromises();
  //   for await (const value of gen2) {
  //     console.log("收到值:", value);
  //   }

  // 测试 3: throw 处理
  console.log("\n--- 测试 3: throw 错误处理 ---");
  const gen3 = generatorWithThrow();
  console.log(await gen3.next());
  try {
    await gen3.throw(new Error("测试错误"));
  } catch (e) {
    console.log("外部捕获:", e.message);
  }

  // 测试 4: return 提前结束
  console.log("\n--- 测试 4: return 提前结束 ---");
  const gen4 = generatorWithReturn();
  console.log(await gen4.next());
  console.log(await gen4.return("提前结束"));
  console.log(await gen4.next()); // 应该返回 done: true

  //   // 测试 5: for-await-of 遍历
  //   console.log("\n--- 测试 5: for-await-of 遍历 ---");
  //   const gen5 = countGenerator();
  //   for await (const num of gen5) {
  //     console.log("计数:", num);
  //   }

  // 测试 6: 嵌套 Generator
  console.log("\n--- 测试 6: 嵌套 Generator (yield*) ---");
  const gen6 = nestedGenerator();
  for await (const value of gen6) {
    console.log("嵌套值:", value);
  }

  //   // 测试 7: 异步迭代器协议
  //   console.log("\n--- 测试 7: 自定义异步迭代器 ---");
  //   for await (const item of asyncIterable) {
  //     console.log("迭代项:", item);
  //   }

  console.log("\n" + "=".repeat(50));
  console.log("所有测试完成!");
  console.log("=".repeat(50));
}

// 执行测试
runTests().catch(console.error);
