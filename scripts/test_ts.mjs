// Dynamic import of typescript.min.js (ESM mode)
const tsModule = await import("./typescript.min.js");
const ts = tsModule.default || tsModule.ts || globalThis.ts;
console.log("ts loaded:", typeof ts);
console.log("ts.transpileModule:", typeof ts?.transpileModule);

if (ts && ts.transpileModule) {
  const code = `
const greeting: string = "hello from TypeScript!";
const add = (a: number, b: number): number => a + b;
console.log(greeting, add(1, 2));
`;
  const result = ts.transpileModule(code, {
    compilerOptions: { target: ts.ScriptTarget.ES2020 },
  });
  console.log("=== Transpiled JS ===");
  console.log(result.outputText);
  console.log("=== Running ===");
  eval(result.outputText);
}
