// 一个依赖无法解析的模块：./missing-helper.ts 不存在
import { helper } from "./missing-helper.ts";
export const value = helper + 1;
