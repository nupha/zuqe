//"debug dump_run_stack";

f = async function* () {};
console.log(f);
console.log(f.constructor);
console.log(Object.getPrototypeOf(f.prototype));
