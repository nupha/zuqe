// Test @babel/standalone for TS transpilation
const tsModule = await import("./babel-standalone.js");
const Babel = globalThis.Babel || tsModule.default;
console.log("Babel loaded:", typeof Babel);
console.log("Babel.transform:", typeof Babel?.transform);

if (Babel && Babel.transform) {
  const code = `
const greeting: string = "hello from TypeScript!";
const add = (a: number, b: number): number => a + b;
console.log(greeting, add(1, 2));
`;
  const result = Babel.transform(code, {
    presets: ["typescript"],
    filename: "test.ts",
  });
  console.log("=== Transpiled JS ===");
  console.log(result.code);
  console.log("=== Running ===");
  eval(result.code);
}
