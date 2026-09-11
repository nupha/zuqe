import * as ns from "./define-own-property.js";
export var local1;
var local2;
export { local2 as renamed };
export { local1 as indirect } from "./define-own-property.js";
var sym = Symbol("test262");

const exported = ["local1", "renamed", "indirect"];
