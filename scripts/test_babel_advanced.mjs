// Test @babel/standalone with advanced TS features
await import("./babel-standalone.js");

const code = `
// Interfaces, generics, enums - all should be stripped
interface User {
  name: string;
  age: number;
  role: Role;
}

enum Role {
  Admin = "admin",
  User = "user",
  Guest = "guest"
}

function greet<T extends { name: string }>(person: T): string {
  return \`Hello, \${person.name}!\`;
}

const admin: User = { name: "Alice", age: 30, role: Role.Admin };
const users: User[] = [
  admin,
  { name: "Bob", age: 25, role: Role.User }
];

type Result<V, E> = { ok: true; value: V } | { ok: false; error: E };

function findUser(name: string): Result<User, string> {
  const found = users.find(u => u.name === name);
  return found
    ? { ok: true, value: found }
    : { ok: false, error: \`User \${name} not found\` };
}

console.log(greet(admin));
console.log(findUser("Alice"));
console.log(findUser("Charlie"));
`;

const result = Babel.transform(code, {
  presets: ["typescript"],
  filename: "test.ts",
});

console.log("=== Transpiled JS ===");
console.log(result.code);
console.log("\n=== Running ===");
eval(result.code);
