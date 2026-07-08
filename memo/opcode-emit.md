# QuickJS vs Zhuque Parser Emit Opcode 对比分析

## 1. 整体架构对比

| 维度                     | QuickJS                                                                                      | Zhuque                                                                                   |
| ------------------------ | -------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------- |
| **编译阶段**             | 三阶段：Parse → `resolve_variables` → `resolve_labels`                                       | 三阶段：Parse → `resolve_variables` → `resolve_labels`（独立调用，见 `mkbc.rs:130-136`） |
| **中间表示**             | 解析时发射完整 final opcode + **temp opcode**（`def`），第三阶段再压缩                       | 解析时发射完整 final opcode（`DEF!`）+ **temp opcode**（`def!`），后两阶段逐步转换       |
| **Temp Opcode 消解时机** | `resolve_labels()` 第三阶段独立处理                                                          | `resolve_labels` 第三阶段独立处理（不是合并到第二阶段）                                  |
| **Temp Opcode 定义**     | **19 个**：label/enter*scope/leave_scope/scope*\_/get\_\_\_opt_chain/set_class_name/line_num | **19 个**：与 QuickJS 完全一致                                                           |
| **源码位置记录方式**     | `source_ptr - buf_start`（源码字节偏移量，存储在 **temp** `line_num` 中）                    | `last_line_num` + `last_line_pos`（行号 i32 + 列号 u32，存储在 **temp** `LINE_NUM` 中）  |
| **字节序**               | Little-endian（`put_u32` packed struct）                                                     | Native endian（`to_ne_bytes()`，x86_64 上即 LE）                                         |

## 2. 核心 Emit 函数对比

| 函数                  | QuickJS                                                                                            | Zhuque                                                                                         |
| --------------------- | -------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------- |
| **发射 opcode 字节**  | `emit_op(s, val)` — 仅写入 1 字节，设置 `last_opcode_pos`（不自动发射 source pos）                 | `emit_op_u8(self, op, line_num)` — 先检查行号变化，自动发射 **temp** `LINE_NUM`，再写入 opcode |
| **发射 temp opcode**  | `emit_op(s, OP_label)` 等 — 统一使用 emit_op                                                       | `emit_op_temp(OpCodeTemp::LABEL)` — 独立函数                                                   |
| **发射 u8/u16/u32**   | `emit_u8(s, val)` / `emit_u16(s, val)` / `emit_u32(s, val)` — 直接调用 `dbuf_putc/put_u16/put_u32` | `emit_u8(val)` / `emit_u16(val)` / `emit_u32(val)` — 调用 `push_vec_u8/u16/u32`                |
| **发射 atom**         | `emit_atom(s, atom)` — 使用 `put_u32` + `JS_DupAtom` 引用计数                                      | `emit_atom(atom)` — 使用 `emit_u32(atom.id())`                                                 |
| **发射 source pos**   | **显式**: `emit_source_pos(s, source_ptr)` 在每处手动调用                                          | **隐式**: 嵌入在每次 `emit_op_u8` 中自动检查并发射                                             |
| **发射 goto**         | `emit_goto(s, opcode, label)`                                                                      | `emit_goto(opcode, label)`                                                                     |
| **发射 label**        | `emit_label(s, label)`                                                                             | `emit_label(label)`                                                                            |
| **发射 push_const**   | `emit_push_const(s, val, as_atom)`                                                                 | `emit_push_const(val, as_atom)`                                                                |
| **检查上一个 opcode** | `get_prev_opcode(fd)` — 读 `byte_code.buf[last_opcode_pos]`                                        | `get_prev_opcode()` — 同样读 `byte_code[last_opcode_pos]`                                      |
| **死代码检测**        | `js_is_live_code(s)`                                                                               | `is_live_code(&self)`                                                                          |

## 3. 源码位置追踪对比

这是**最显著的设计差异**:

```c
// QuickJS — 显式调用，emit OP_line_num (temp) + u32 字节偏移
static void emit_source_pos(JSParseState *s, const uint8_t *source_ptr) {
    if (fd->last_opcode_source_ptr != source_ptr) {
        dbuf_putc(bc, OP_line_num);                    // temp opcode
        dbuf_put_u32(bc, source_ptr - s->buf_start);   // 字节偏移
        fd->last_opcode_source_ptr = source_ptr;
    }
}

// 解析时每处手动调用:
emit_source_pos(s, op_token_ptr);
emit_op(s, OP_neg);
```

```rust
// Zhuque P1 后 — 隐式双轨：LINE_NUM 同时携带行列
pub(crate) fn emit_op_u8(&mut self, op: u8, line_num: i32, line_pos: u32) {
    // 行号或列号任一变化时 emit
    if self.last_opcode_line_num != line_num || self.last_opcode_line_pos != line_pos {
        push_vec_u8(bc, OpCodeTemp::LINE_NUM as u8);
        push_vec_u32(bc, line_num as _);   // 行号
        push_vec_u32(bc, line_pos);        // 列号（字节列）
        self.last_opcode_line_num = line_num;
        self.last_opcode_line_pos = line_pos;
    }
    self.last_opcode_pos = bc.len() as i32;
    bc.push(op);
}

// 解析时不需要手动调用，emit_op 自动处理
self.emit_op(OpCode::NEG);
```

> **pc2line 编码**：对齐 QuickJS（`quickjs.c:34494`），每个 entry 后始终写 `SLEB128(diff_col)`，不区分短/长编码。
> **异常栈**：`Error.stack` 输出 `filename:line:col` 格式。

| 特性         | QuickJS                                                          | Zhuque                                                                                 |
| ------------ | ---------------------------------------------------------------- | -------------------------------------------------------------------------------------- |
| **调用方式** | 显式（需手动调用 `emit_source_pos`）                             | 隐式（嵌入 `emit_op_u8`）                                                              |
| **记录值**   | `source_ptr - buf_start`（源码字节偏移，运行时扫描换行得行列号） | **双值**：`LINE_NUM` 同时存行号 i32 + 列号 u32（P1 后，运行时直接可用）                |
| **去重条件** | `last_opcode_source_ptr != source_ptr`（指针比较）               | `last_opcode_line_num != line_num OR last_opcode_line_pos != line_pos`（行列联合判断） |
| **优点**     | 精准到字节偏移，可精确还原行号+列号                              | 行号列号直接可用（无需运行时扫描源码、无需持有源码）                                   |
| **缺点**     | 容易遗漏调用，运行时需扫描源码 O(n)                              | LINE_NUM 从 5B 扩为 9B（行列两值）                                                     |
| **调用频率** | 仅关键位置（运算符、return、throw 等）                           | 每个 opcode 调用时自动检查                                                             |

## 4. Opcode 编码格式对比

| 格式字符串       | QuickJS                                                                                                | Zhuque                                                                                                          |
| ---------------- | ------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------- |
| **定义位置**     | `quickjs-opcode.h` 中 `DEF/def` 宏的第5参数（单字符: `none`, `i32`, `atom`, `label`, `loc`, `u16` 等） | `build_opcodes.rs` 中 `DEF!/def!` 宏的 `fmt` 字段（多字符: `"NONE"`, `"I32"`, `"ATOM"`, `"LABEL"`, `"LOC"` 等） |
| **格式种类数**   | 约 12 种                                                                                               | 约 22 种                                                                                                        |
| **编码方式**     | 所有 int 参数使用 fixed-width LE                                                                       | 使用 native endian 固定宽度                                                                                     |
| **Atom 编码**    | 4 字节 LE + `JS_DupAtom` 引用计数                                                                      | 4 字节 NE + `atom.id()`                                                                                         |
| **LABEL 编码**   | 4 字节 LE label 索引 → 第二阶段解析为相对偏移                                                          | 4 字节 NE label 索引 → resolve 时解析                                                                           |
| **特殊格式**     | `npop` 用 u16 编码参数数量                                                                             | `NPOP` 用 u16, `NPOP_U16` 额外加 u16 参数                                                                       |
| **Inline Cache** | 无专用格式                                                                                             | `ATOM_U16` 格式用于 `GET_FIELD_IC`/`GET_FIELD2_IC`/`PUT_FIELD_IC`（7 字节，含 ic_index）                        |

## 5. Label 处理对比

> **`OP_label` (`LABEL` 是 temp opcode**（ `def` / `def!`），在 QuickJS phase 3 和 Zhuque resolve_labels 中被消解，不出现在最终字节码。

| 特性               | QuickJS                                                                         | Zhuque                                                |
| ------------------ | ------------------------------------------------------------------------------- | ----------------------------------------------------- |
| **Label 分配**     | `new_label(s)`                                                                  | `add_label(&mut self)`                                |
| **Label 槽位**     | `JSFunctionDef::label_slots[]`（含 `pos`, `ref_count`）                         | `FunctionDef::label_slots[]`（含 `pos`, `ref_count`） |
| **emit_goto 行为** | 基于 `js_is_live_code` 条件发射 — goto/label 均为 **temp** 阶段的中间指令       | 基于 `is_live_code` 条件发射                          |
| **死亡代码处理**   | 若死代码，返回 -1 不发射                                                        | 若死代码，同样返回 -1                                 |
| **Label 解析阶段** | `resolve_labels()` 第三阶段，扫描 byte_code 替换 **temp** `OP_label` 为实际偏移 | 在 `resolve_labels` 中一并处理 **temp** `op_label`    |
| **跳转优化**       | `resolve_labels` 中: 跳转链折叠、消除冗余跳转、跟随跳转目标                     | 已在 `resolve_labels` 中实现                          |

## 6. 短 Opcode 处理对比

| 特性               | QuickJS                                                                      | Zhuque                                                                                              |
| ------------------ | ---------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------- |
| **短 opcode 定义** | `quickjs-opcode.h` 中用 `DEF` 宏定义，`#if SHORT_OPCODES` 开关               | `build_opcodes.rs` 中已定义但**全部注释掉**                                                         |
| **压缩时机**       | `resolve_labels` 第三阶段用 `put_short_code()` / `push_short_int()` 正则发射 | `push_vec_short_code()` / `push_vec_short_int()` 函数已实现但**仅在 `short_opcode` feature 下编译** |
| **整数压缩**       | `push_i32 (-1..7)` → 单字节；`i8` → 2B；`i16` → 3B；否则 `push_i32` 5B       | 同等方案（`OP_push_0 + val` / `OP_push_i8` / `OP_push_i16` / `OP_push_i32`）                        |
| **变量访问压缩**   | `get_loc0..3` → 1B；`get_loc8` → 2B；否则 `get_loc` + u16 → 3B               | 同等方案 behind `short_opcode` feature                                                              |
| **跳转压缩**       | `goto8`(2B), `goto16`(3B), `if_false8`(2B)                                   | `goto8`(2B), `goto16`(3B) behind feature                                                            |
| **默认状态**       | **启用**（SHORT_OPCODES=1）                                                  | **禁用**（全部注释，feature 未激活）                                                                |

## 7. Parser 调用 Emit 的模式差异

| 操作                 | QuickJS emit 模式                                                                | Zhuque emit 模式                                       |
| -------------------- | -------------------------------------------------------------------------------- | ------------------------------------------------------ |
| **一元运算符 `-x`**  | `emit_source_pos(s, ptr)` + `emit_op(s, OP_neg)`                                 | `emit_op(OpCode::NEG)`                                 |
| **二元运算符 `a+b`** | `parse_expr_binary(L-1)` + `emit_source_pos(s, ptr)` + `emit_op(s, OP_add)`      | `parse_expr_binary(L-1)` + `emit_op(OpCode::ADD)`      |
| **属性访问 `a.b`**   | `emit_source_pos` + `emit_op(OP_get_field)` + `emit_atom(name)`                  | `emit_get_field(atom)` — 可能在有 feature 时发 IC 变体 |
| **函数调用**         | `emit_op(OP_call)` + `emit_u16(arg_count)`                                       | `emit_op(CALL)` + `emit_u16(arg_count)`                |
| **三元表达式**       | `emit_goto(OP_if_false, -1)` + ... + `emit_goto(OP_goto, -1)` + `emit_label(l1)` | 同等模式                                               |
| **逻辑 `&&`**        | `emit_op(OP_dup)` + `emit_goto(OP_if_false, -1)` + `emit_op(OP_drop)`            | 同等模式                                               |
| **Return**           | `emit_source_pos(s, ptr)` + `emit_return(s, hasval)`                             | `emit_return(hasval)`                                  |
| **Throw**            | `emit_source_pos(s, ptr)` + `emit_op(OP_throw)`                                  | `emit_op(THROW)`                                       |

## 8. 关键差异总结表

| 序号 | 差异点                  | QuickJS                                 | Zhuque                                                                | 说明                                                  |
| ---- | ----------------------- | --------------------------------------- | --------------------------------------------------------------------- | ----------------------------------------------------- |
| 1    | **Source pos 发射方式** | 显式 `emit_source_pos()` 手动调用       | 隐式嵌入 `emit_op_u8`（**temp** `LINE_NUM` 存行列双值）               | Zhuque 更简洁，P1 后同时具备行列精度                  |
| 2    | **Source pos 内容**     | 字节偏移 → 运行时扫描换行得行列号       | 行号 i32 + 列号 u32（直接可用，无需运行时扫描源码）                   | Zhuque 更高效（O(1) vs O(n) 取值）                    |
| 3    | **LINE_NUM size**       | 5B (1B opcode + 4B offset)              | 9B（1B opcode + 4B line + 4B col）                                    | Zhuque 多 4B 换取行列直读                             |
| 3    | **编译阶段数**          | 3 阶段（每个 pass 独立）                | 3 阶段（每个 pass 独立，resolve_labels 为独立顶层调用）               | 结构等价                                              |
| 4    | **短 opcode 默认状态**  | **开启**                                | **关闭**（全部注释，本次不涉及）                                      | QuickJS 默认生成更紧凑字节码                          |
| 5    | **Endianness**          | 固定 Little-endian                      | Native endian                                                         | x86_64 上二者等同                                     |
| 6    | **Inline Cache**        | 无                                      | **有** feature 封装，IC 在解析时发射                                  | Zhuque 有额外属性访问优化                             |
| 7    | **Atom Opcode 格式**    | 仅 `ATOM`（4B）                         | `ATOM`、`ATOM_U8`、`ATOM_U16`、`ATOM_LABEL_U8`、`ATOM_LABEL_U16` 多种 | Zhuque 格式更丰富（含 **temp** scope\_\* 的复合格式） |
| 8    | **Format 种类数**       | ~12 种                                  | ~22 种                                                                | Zhuque 编码格式更精细                                 |
| 9    | **死代码检测方式**      | `js_is_live_code(s)`                    | `is_live_code(&self)`                                                 | 逻辑等价                                              |
| 10   | **整数编码优化**        | `resolve_labels` 阶段做                 | `push_vec_short_int` 已实现但**不用**                                 | QuickJS 实际产生更小字节码（本次不涉及）              |
| 11   | **变量访问优化**        | `resolve_labels` 阶段 `put_short_code`  | 已实现但**不用**                                                      | 同上                                                  |
| 12   | **parser 端接口封装**   | `JSParseState` 直接操作 `fd->byte_code` | `JSParseState` + `FunctionDef` 双层封装                               | Zhuque 接口更结构化                                   |
| 13   | **Temp opcode 定义**    | 19 个（`def`）                          | 19 个（`def!`）                                                       | **完全一致**                                          |
| 14   | **废弃 final opcode**   | 无                                      | 6 个（P5 移除）                                                       | 对齐后消除冗余                                        |

## 9. 字节码定义一致性对比

两份字节码定义存在一定量的差异，但不影响语义完整性。以下按类别列出所有不一致之处。

### 9.1 QuickJS 有 → Zhuque 没有的 Common Opcode

| 序号 | Opcode              | QuickJS                     | Zhuque               | 对齐状态                                      |
| ---- | ------------------- | --------------------------- | -------------------- | --------------------------------------------- |
| 1    | `init_ctor`         | 1B, n_pop=0, n_push=1, none | **已添加** — P2 定义 | ✅ parser 已发射（默认构造函数 P4.1）         |
| 2    | `set_loc_check`     | 3B, n_pop=1, n_push=1, loc  | **已添加** — P2 定义 | ✅ 已发射（resolve_scoped_vars 中 P4.2 完成） |
| 3    | `for_await_of_next` | 1B, n_pop=3, n_push=4, none | **已添加** — P2 定义 | ✅ parser 已发射（async for-of P4.3）         |
| 4    | `push_bigint_i32`   | 5B, n_pop=0, n_push=1, i32  | **已添加** — P2 定义 | ⚠️ 有定义+unreachable handler，未启用         |

### 9.2 Zhuque 有 → QuickJS 没有的 Common Opcode

| 序号 | Opcode             | Zhuque       | QuickJS                                         | 对齐后状态（代码实际状态）                           |
| ---- | ------------------ | ------------ | ----------------------------------------------- | ---------------------------------------------------- |
| 1    | `check_var`        | 5B, ATOM     | 无（由 `GET_VAR_UNDEF` 内嵌）                   | **P5 已移除** — 定义+runtime handler 已删除          |
| 2    | `put_var_strict`   | 5B, ATOM     | 无（`PUT_VAR` + `is_strict_mode` 运行时判断）   | **P5 已移除** — 定义+runtime handler 已删除          |
| 3    | `define_var`       | 6B, ATOM_U8  | 无（`scope_put_var_init` temp 在 resolve 消化） | **P5 已移除** — 定义+runtime handler 已删除          |
| 4    | `check_define_var` | 6B, ATOM_U8  | 无（同上）                                      | **P5 已移除** — 定义+runtime handler 已删除          |
| 5    | `define_func`      | 6B, ATOM_U8  | 无（`FCLOSURE` + `PUT_VAR`/`PUT_LOC`）          | **P5 已移除** — 定义+runtime handler 已删除          |
| 6    | `get_field_ic`     | 7B, ATOM_U16 | 无                                              | **保留** — inline cache 核心优化，benchmark 验证有效 |
| 7    | `get_field2_ic`    | 7B, ATOM_U16 | 无                                              | **保留**                                             |
| 8    | `put_field_ic`     | 7B, ATOM_U16 | 无                                              | **保留**                                             |

### 9.3 Opcode Size / 编码格式不同（同名 opcode）

| 序号 | Opcode          | QuickJS           | Zhuque             | 说明                                                     |
| ---- | --------------- | ----------------- | ------------------ | -------------------------------------------------------- |
| 1    | `get_var_undef` | 3B (1+2), var_ref | **5B** (1+4), ATOM | QuickJS 用 2 字节 var_ref 索引，Zhuque 用 4 字节 atom id |
| 2    | `get_var`       | 3B (1+2), var_ref | **5B** (1+4), ATOM | 同上                                                     |
| 3    | `put_var`       | 3B (1+2), var_ref | **5B** (1+4), ATOM | 同上                                                     |
| 4    | `put_var_init`  | 3B (1+2), var_ref | **5B** (1+4), ATOM | 同上                                                     |

> **注**：这 4 个 opcode 在两者中都是 resolve 阶段的最终产出，差异根源在于 QuickJS 在 resolve 后将 `scope_get_var` / `scope_put_var` 转换为紧凑的 `var_ref` (2B) 索引，而 Zhuque 保留为 `ATOM` (4B) 形式。

### 9.4 Stack Effect 语义不同（同名 opcode）

| 序号 | Opcode             | QuickJS                 | Zhuque                  | 说明                                                            |
| ---- | ------------------ | ----------------------- | ----------------------- | --------------------------------------------------------------- |
| 1    | `import`           | n_pop=**2**, n_push=1   | n_pop=**2**, n_push=1   | ✅ **P1.2 已修复**，压入 specifier + options（undefined）两个值 |
| 2    | `call_constructor` | n_pop=2, n_push=1, npop | n_pop=2, n_push=1, NPOP | 语义相同，记法对齐                                              |
| 3    | `special_object`   | 2B, u8                  | 2B, U8                  | 格式相同                                                        |

### 9.5 短 Opcode 差异

| 序号 | Short Opcode                                                   | QuickJS                  | Zhuque       | 说明               |
| ---- | -------------------------------------------------------------- | ------------------------ | ------------ | ------------------ |
| 1    | `push_minus1` ~ `push_7` (9个)                                 | `#if SHORT_OPCODES` 激活 | **全部注释** | Zhuque 暂未启用    |
| 2    | `push_i8` / `push_i16`                                         | 激活                     | 注释         | 同上               |
| 3    | `push_const8` / `fclosure8`                                    | 激活                     | 注释         | 同上               |
| 4    | `push_empty_string`                                            | 激活                     | 注释         | 同上               |
| 5    | `get_loc0..3`, `put_loc0..3`, `set_loc0..3` (12个)             | 激活                     | 注释         | 同上               |
| 6    | `get_loc8`, `put_loc8`, `set_loc8` (3个)                       | 激活                     | 注释         | 同上               |
| 7    | `get_arg0..3`, `put_arg0..3`, `set_arg0..3` (12个)             | 激活                     | 注释         | 同上               |
| 8    | `get_var_ref0..3`, `put_var_ref0..3`, `set_var_ref0..3` (12个) | 激活                     | 注释         | 同上               |
| 9    | `get_length`                                                   | 激活                     | 注释         | 快速获取 `.length` |
| 10   | `if_false8`, `if_true8`, `goto8`, `goto16` (4个)               | 激活                     | 注释         | 短跳转             |
| 11   | `call0..3` (4个)                                               | 激活                     | 注释         | 短调用             |
| 12   | `is_undefined`, `is_null`                                      | 激活                     | 注释         | 快速类型检查       |
| 13   | `typeof_is_undefined`, `typeof_is_function`                    | 激活                     | 注释         | 快速 typeof 检查   |

### 9.6 不一致性汇总

| 类别                             | 数量  | 严重程度                                             |
| -------------------------------- | ----- | ---------------------------------------------------- |
| QuickJS 独有 opcode（已对齐）    | 0     | ✅ 已全部添加（P2），部分 parser 路径待 P4 启用      |
| Zhuque 独有 opcode（IC 保留）    | 3     | 低（inline cache 优化）                              |
| Zhuque 独有 opcode（对齐后废弃） | **6** | **P5 已清理** — 全部 6 个定义+runtime handler 已删除 |
| 编码格式不同                     | 4     | ✅ **P3 已解决**（var_ref 对齐 QuickJS）             |
| stack effect 不同                | 0     | ✅ **P1.2 已修复**（import 的 n_pop）                |

**结论**：两份字节码定义总体一致，核心差异集中在：

1. ~~QuickJS 的 `get_var` / `put_var` 等使用更紧凑的 `var_ref` (2B) 格式，Zhuque 使用 `ATOM` (4B)~~ ✅ **P3 已对齐**，全局变量全部使用 var_ref（3B）
2. Zhuque 多了 inline cache 系列 opcode（保留）和 6 个变量定义系列 opcode（对齐后废弃，含 `with_get_ref_undef`）
3. ~`import` 的 stack effect 定义不同~ ✅ **P1.2 已修复**
4. 短 opcode 在 Zhuque 中已写好但未激活

## 10. 结论

**QuickJS** 的设计更"传统编译器"风格：

- 三阶段编译，`resolve_labels` 是独立的优化 pass
- 显式 source pos 发射让 parser 有完全控制权，且记录字节偏移可精确到列
- 短 opcode 默认激活，产出更紧凑的字节码
- 变量访问用 `var_ref` (2B) 格式，字节码更小

**Zhuque** 的设计更"精简"：

- 三阶段编译，`resolve_labels` 为独立顶层调用（`mkbc.rs:136`）
- 隐式行号追踪简化 parser 代码（无需手动 `emit_source_pos` 调用）
- 格式系统更丰富（`ATOM_U8`、`ATOM_U16`、`ATOM_LABEL_U8` 等），部分合并了 QuickJS 中 **temp** opcode 的参数
- Inline cache 机制为属性访问提供额外优化通道
- 短 opcode 系统已实现但尚未激活（本次不涉及）
- 变量访问用 `ATOM` (4B) 格式，更统一但体积略大

最核心的设计哲学差异在于：**QuickJS 在第三阶段集中做优化和压缩**，而 **Zhuque 将这些分散到解析时选择不同 opcode（如 IC variant）和 resolve 阶段的转换中**。

**对齐后**：Zhuque 已合并 QuickJS 的 var_ref 运行时模型（代码密度 + 执行效率双赢），同时保留自身的 Inline Cache 和隐式 LINE_NUM 优势。**Temp opcode** 体系已完全一致无需改动。6 个对齐后冗余的 **final** opcode（含 `with_get_ref_undef`，详见 13.7 节）已在 P5 中统一移除。

---

## 11. 字节码对齐实施计划

### 设计原则

1. **Source pos 沿用隐式机制**：不拆掉现有的 LINE_NUM 自动发射机制，只新增 `last_line_pos` 字段记录当前行的列位置（字节列），tokenizer 逐字符增量维护（遇 `\n` 归零）。345 处 `emit_op` 调用无需改动。
2. **各阶段去耦合、独立验证**：每个阶段修改一个垂直领域（opcode 定义 → resolve 阶段 → 运行时），且可独立通过 test262 验证。
3. **short_opcode 暂不实现**。

### 核心改动思路

**Source pos 追踪**：**不替换** `last_line_num`，而是**新增** `last_line_pos` 字段，两个字段并行。`last_line_num` 继续用于 LINE_NUM 行号发射，`last_line_pos` 另存当前列的字节位置。tokenizer 逐字符增量维护（每读一个字符 `+1`，遇 `\n` 归零）。这样 345 处 `emit_op` 零改动，运行时直接拿到 (line, col) 无需任何转换。

```
tokenizer: last_line_num <-- 遇 '\n' 时 +1           // 保留，emit LINE_NUM
           last_line_pos <-- 逐字符 +1，遇 '\n' 归零   // 新增，存入 LINE_NUM 参数

emit_op_u8: if line_num 变了 → emit LINE_NUM + (line_num, line_pos)  // 行列一起
```

**为什么选 `last_line_pos` 而不是 `last_source_pos`（字节 offset）**：

| 方案                       | tokenizer 维护                            | 运行时取列号                    | 需要源码 |
| -------------------------- | ----------------------------------------- | ------------------------------- | -------- |
| `last_source_pos` → 列号   | `last_ptr - input.as_ptr()`（每次算减法） | O(source_len) 扫描逐字节数 `\n` | **是**   |
| **`last_line_pos` → 列号** | **逐字符 `+1`，遇 `\n` 归零**（无减法）   | **O(1)，已在 opcode 里**        | **否**   |

`last_line_pos` 与现有的 `last_line_num` 完全对称——都是逐字符增量维护，tokenizer 改动极小，运行时零开销，不需要运行时持有源码文本。

---

### 阶段划分总表

| 阶段   | 名称                     | 修改内容概要                                                                                                                                | 验证方式                         | 状态          |
| ------ | ------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------- | ------------- |
| **P1** | Source pos + import 修复 | `LINE_NUM` (**temp**) 扩展为存 (line_num, line_pos)；import (**final**) 修复 n_pop=2                                                        | test262 回归；栈追踪行列信息正确 | ✅ **已完成** |
| **P2** | 新增 opcode 定义         | build_opcodes.rs 加 4 个 **final** opcode，运行时加 stub handler                                                                            | 编译通过，test262 回归           | ✅ **已完成** |
| **P3** | var_ref 格式迁移         | Resolve 阶段改 emit var_ref；运行时改 var_ref 寻址                                                                                          | test262 全局变量 + 控制流测试    | ✅ **已完成** |
| **P4** | Parser 配套改动          | super() 发 INIT_CTOR (**final**)；for-await 改发 FOR_AWAIT_OF_NEXT (**final**)                                                              | test262 class/async 测试         | ✅ **已完成** |
| **P5** | 废弃 opcode 清理         | 删除 6 个 **final** opcode（check_var/put_var_strict/define_var/check_define_var/define_func/with_get_ref_undef），resolve 与运行时路径清理 | test262 全面回归                 | ✅ **已完成** |

> **Temp opcode 已对齐**：`def!`（OpCodeTemp）级别，Zhuque 与 QuickJS 的 19 个 temp opcode **完全一致**（`with_get_ref_undef` 是 final opcode，非 temp，见 13.7 节）。short_opcode 暂不涉及，将来独立追加。

**opcode 分类标识**：全文中标注 **(temp)** 表示临时 opcode（`def!`/`OpCodeTemp`），在 resolve 阶段被消解；标注 **(final)** 表示最终 opcode（`DEF!`/`OpCode`），保留到运行时。

---

### P1：Source pos + import 修复

**目的**：扩展 `LINE_NUM` **temp** opcode 使其同时携带行号和列号（对 齐 QuickJS 的列级精度），同时保留现有行号路径零开销。修复 `import` 的 n_pop 不一致。

#### P1.1 Source pos 追踪改造 ✅ 已完成

> **设计原则**：新增而非替换。`last_line_num` → `LINE_NUM` 路径不动，新增 `last_line_pos` 字段作为 LINE_NUM 的第二个参数（存列号）。

| 文件                             | 修改内容                                                                                                                                                                                               | 说明                                                            |
| -------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------- |
| `src/parse/state.rs`             | 新增 `last_line_pos: u32` 字段；`JSParsePos` 同步扩展；新增 `line_start_offset: usize`                                                                                                                 | 当前 token 所在行的列位置（字节列）。**不删除** `last_line_num` |
| `src/parse/tokenize.rs`          | `next_token()` 开始时从 `token.ptr` 计算 `last_line_pos`；遇 `\r`/`\n`/`\u{2028}`/`\u{2029}` 更新 `line_start_offset`                                                                                  | 与 `last_line_num` 并列维护                                     |
| `src/function/func_def.rs`       | 新增 `last_opcode_line_pos: u32` 字段；`LineNumberSlot` 增加 `line_pos: u32`；`add_pc2line_info` 增加 `line_pos` 参数                                                                                  | **不删除** `last_opcode_line_num`，两个字段并存                 |
| `src/function/emit.rs`           | `emit_op_u8` 中：LINE_NUM 检查改为**同时检查** `last_opcode_line_num != line_num OR last_opcode_line_pos != line_pos`；所有 emit 方法签名增加 `line_pos` 参数；`push_vec_sleb128` 改为标准 ZigZag 编码 | 去重条件从单值变为双值组合                                      |
| `src/function/scope.rs`          | `push_scope`/`pop_scope`/`close_scopes` 增加 `line_pos` 参数                                                                                                                                           |                                                                 |
| `build_opcodes.rs`               | `line_num` 格式从 `"U32"`(5B) 改为 `"U32_U32"`(9B)；新增 `U32_U32` 到 `OpcodeFormat` 枚举                                                                                                              | LINE_NUM = 1B opcode + 4B line_num + 4B line_pos                |
| `src/parse/emit.rs`              | 所有 emit wrapper 捕获 `self.last_line_pos` 并传递                                                                                                                                                     | 函数签名增加 `line_pos` 参数                                    |
| `src/function/resolve_vars.rs`   | 所有 `add_pc2line_info` 调用增加 `line_pos` 参数；`no_change!` 宏使用 `op_len`（9B）自动处理；LINE_NUM 手动写入附带 line_pos                                                                           |                                                                 |
| `src/function/resolve_labels.rs` | `line_num` match arm 读 `line_num`+`line_pos`；所有 `add_pc2line_info` + optimize 调用增加 `line_pos` 参数                                                                                             |                                                                 |
| `src/function/optimize.rs`       | 所有 optimize 函数（`optimize_get_loc` 等）增加 `line_pos: &mut u32` 参数                                                                                                                              |                                                                 |
| `src/function/label.rs`          | `compute_pc2line_info`：跳过条件改为 `diff_line==0 && diff_line_pos==0`（对齐 QuickJS）；**短编码后也写** `SLEB128(diff_line_pos)`（对齐 QuickJS 34494 行）                                            | 列增量统一在 if-else 之外写                                     |
| `src/function/bytecode.rs`       | 新增 `find_line_num_col()` 返回 `(line_num, line_pos)`；解码时**每个 entry 后都读** `SLEB128(line_pos_delta)`（对齐 QuickJS 7482 行）                                                                  | `find_line_num()` 改为委托 `find_line_num_col()`                |
| `src/bytecode/dump.rs`           | 新增 `U32_U32` 格式 dump 支持                                                                                                                                                                          | 输出 `line X col Y`                                             |
| `src/error.rs`                   | 异常栈输出改为 `filename:line:col` 格式                                                                                                                                                                | 调用 `find_line_num_col()` 获取列号                             |
| `src/parse/expr_cond.rs`         | 直接 FunctionDef emit 调用补 `line_pos` 参数                                                                                                                                                           |                                                                 |
| `src/parse/function.rs`          | 直接 FunctionDef emit 调用补 `line_pos` 参数                                                                                                                                                           |                                                                 |
| `src/eval.rs`                    | `push_scope` 调用补 `line_pos` 参数                                                                                                                                                                    |                                                                 |

**涉及 opcode**：`LINE_NUM`（temp，格式从 5B 扩展为 9B，增加 line_pos 参数）

**验证**：test262 回归 — `last_line_num` 路径零改动，stack trace 行列信息均可直接获取（无需运行时从 offset 扫描）

#### P1.2 import 修复 ✅ 已完成

| 文件                             | 修改内容                                                           | 涉及 opcode           |
| -------------------------------- | ------------------------------------------------------------------ | --------------------- |
| `build_opcodes.rs`               | `import` n_pop: 1 → **2**                                          | `import`              |
| `src/parse/expr.rs`              | import() 解析前 emit `UNDEFINED` 作为 options 参数                 | `UNDEFINED`, `IMPORT` |
| `src/bytecode/exec_stackless.rs` | IMPORT handler：从栈取 specifier(sp[-2]) 和 options(sp[-1])，pop 2 | `IMPORT`              |
| `src/module/import.rs`           | `js_dynamic_import` 函数签名增加 `_options` 参数                   | —                     |

**验证**：`import().then(success, fail)` 正常回调，字节码 dump 确认 `UNDEFINED → IMPORT` 序列正确，test262 无新增失败

---

### P2：新增 opcode 定义

**目的**：补齐 QuickJS 有而 Zhuque 缺少的 opcode（仅添加定义 + 运行时 stub handler，不涉及 parser 改变）。

#### P2.1 build_opcodes.rs

| 新增 opcode         | size | n_pop | n_push | format | 位置                    |
| ------------------- | ---- | ----- | ------ | ------ | ----------------------- |
| `init_ctor`         | 1    | 0     | 1      | NONE   | check_ctor 之后         |
| `set_loc_check`     | 3    | 1     | 1      | LOC    | put_loc_check_init 之后 |
| `for_await_of_next` | 1    | 3     | 4      | NONE   | for_of_next 之后        |
| `push_bigint_i32`   | 5    | 0     | 1      | I32    | nop 之前                |

#### P2.2 运行时 stub handler

| 文件                             | 添加内容                                                  |
| -------------------------------- | --------------------------------------------------------- |
| `src/bytecode/exec_stackless.rs` | 4 个 match arm，初始行为：`unreachable!()` 或等效已有逻辑 |

`init_ctor` 和 `set_loc_check` 在已有 resolve/parser 路径补全前不会执行到，`for_await_of_next` 同理。`push_bigint_i32` 由 `emit_push_const` 替代。

**验证**：编译通过，test262 回归（新增 opcode 未被 emit，不影响现有路径）

> **✅ P2 已完成**（2026-07-06）。此外，在 `build_opcodes.rs` 中为所有存在 `op + N → op2` 数值依赖的指令族添加了 `// START/END OF BYTECODE GROUP: XXXX` 边界注释，共 7 组（VAR_ACCESS / VARIABLE_TRIPLE / VAR_REF_CHECK_TRIPLE / BRANCH / WITH_VAR / INC_DEC / SCOPE_VAR），防止未来新增 opcode 时误插入指令族内部。

---

### P3：var_ref 格式迁移 ✅ 已完成

**目的**：`get_var`/`put_var`/`put_var_init`/`get_var_undef` 从 ATOM(5B) 改为 VAR_REF(3B)，对齐 QuickJS 的间接寻址模型。

**核心思路**：Zhuque 已有成熟的 VarRef 基础设施（`VarRefImpl::Bind { pvalue }` + `value()`/`set_value()`/`detach()`），闭合变量已通过 `captured_vars[idx]` 用此模型运行。全局变量只需扩展同一个数组/链路，即可零新增概念地得到 QuickJS 的 `var_refs[idx]->pvalue` 模式。

#### 当前 vs 目标对比

```
当前 GET_VAR:  1B opcode + 4B atom_id + realm_scope.get_global_var(atom)
                                  ↓
目标 GET_VAR:  1B opcode + 2B var_ref_idx + captured_vars[idx].value()
               (回退: uninitialized → fallback to realm_scope)
```

运行时完全复用现有的 `captured_vars: &mut [VarRef]` 切片（从 `bfun.payload_mut().closure_var_refs_mut()` 获取）。

#### P3.1 Opcode 定义层

| 文件               | 修改内容                                                         | 涉及 opcode     |
| ------------------ | ---------------------------------------------------------------- | --------------- |
| `build_opcodes.rs` | 新增 `"VAR_REF"` 格式到 fmts 数组（`OpcodeFormat::VarRef` 变体） | —               |
| `build_opcodes.rs` | `get_var_undef`: 5B ATOM → **3B VAR_REF**                        | `get_var_undef` |
| `build_opcodes.rs` | `get_var`: 5B ATOM → **3B VAR_REF**                              | `get_var`       |
| `build_opcodes.rs` | `put_var`: 5B ATOM → **3B VAR_REF**                              | `put_var`       |
| `build_opcodes.rs` | `put_var_init`: 5B ATOM → **3B VAR_REF**                         | `put_var_init`  |

保持不变：`check_var`/`put_var_strict`/`define_var`/`check_define_var`/`define_func` **不在此转换范围内**（对齐后这些 opcode 将整体废弃，由 QuickJS 路径替代，见第 11 节末尾）；`delete_var` 保持 ATOM 不变（QuickJS 也是 ATOM）

#### P3.2 FunctionDef 数据层

利用现有的 `ClosureVar` 结构（已有 `var_name`、`is_lexical`、`is_const`），不新增类型：

| 文件                       | 修改内容                                                                                           | 复用已有                                           |
| -------------------------- | -------------------------------------------------------------------------------------------------- | -------------------------------------------------- |
| `src/function/func_def.rs` | 新增 `global_var_refs: Vec<ClosureVar>` 字段（编译期记录"此函数引用了哪些全局变量"）               | `ClosureVar` 结构复用                              |
| `src/function/func_def.rs` | 新增 `get_or_add_global_var_ref(&mut self, name, is_lexical, is_const) -> u16` 方法                | 类似已有 `get_closure_var()`                       |
| `src/function/bytecode.rs` | `Bytecode` 新增 `global_var_ref_start`/`global_var_ref_count` 字段，序列化到 binary                | 类似已有的 `closure_var_start`/`closure_var_count` |
| `src/function/bfun.rs`     | `BFuncPayload` 的 `closure_var_refs` 数组扩展：**前 N 个为全局变量 VarRef，后续为闭包变量 VarRef** | 复用 `closure_var_refs: Vec<VarRef>`               |

索引映射：`var_ref_idx = 0..global_var_count - 1` 映射到 `captured_vars[0..N-1]`，闭包变量从 `N` 开始。

#### P3.3 Emit 函数层（最小改动）

既然 opsize 从 5B 变 3B 是由 `resolve_scoped_vars` 直接在输出缓冲区里写 u16 vs u32 决定，不需要新增 emit 函数。只需调整 resolve 的输出方式：

```rust
// 修改前：输出 ATOM（5B）
bc_out.push(opcode);              // 1B
bc_out.extend(u32::to_ne_bytes(atom.id()));  // 4B

// 修改后：输出 VAR_REF（3B）
bc_out.push(opcode);              // 1B
bc_out.extend(u16::to_ne_bytes(var_ref_idx));  // 2B
```

#### P3.4 Resolve 阶段

| 文件                                  | 修改内容                                                                                                                  | 涉及 opcode                                           |
| ------------------------------------- | ------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------- |
| `src/function/resolve_scoped_vars.rs` | `resolve_scope_var_global()`：调用 `fd.get_or_add_global_var_ref(name, ..)` 获取 idx，写 `u16(idx)` 替代 `u32(atom.id())` | `GET_VAR`, `GET_VAR_UNDEF`, `PUT_VAR`, `PUT_VAR_INIT` |
| `src/function/resolve_scoped_vars.rs` | `resolve_scope_var` → `scope_delete_var`：保持 ATOM 不变                                                                  | `DELETE_VAR`                                          |

#### P3.5 运行时 — 函数初始化（创建全局 VarRef）

| 文件/函数                                                      | 修改内容                                                                                                                                                                    |
| -------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `src/function/make_closure_for_arena`（bfun.rs 或 closure.rs） | **新增函数**：遍历 `bytecode.global_var_refs()`，为每个全局变量创建 `VarRef::Detached(JSValue::Uninitialized)` 或 `VarRef::Bind` 指向初始化槽，插入 `closure_var_refs` 头部 |
| 或复用 `BFuncPayload::new()` 初始化                            | 构造 `closure_var_refs` 时前 `global_var_count` 个用 `VarRef::create(ctx, JSValue::Uninitialized)`                                                                          |

#### P3.6 运行时解释器 — 复用 captured_vars

| 文件                             | 修改内容                                                                                                                                                                                                            | 涉及 opcode                |
| -------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------- |
| `src/bytecode/exec_stackless.rs` | `GET_VAR`/`GET_VAR_UNDEF`：`let idx = PC_FETCH!(u16); let val = captured_vars[idx].value();` — 若 Uninitialized，fallback 到 `realm_scope.get_global_var(cv.var_name)` — **完全复用现有 `captured_vars[idx]` 模式** | `GET_VAR`, `GET_VAR_UNDEF` |
| `src/bytecode/exec_stackless.rs` | `PUT_VAR`/`PUT_VAR_INIT`：同路径，`captured_vars[idx].set_value(rt, val)`                                                                                                                                           | `PUT_VAR`, `PUT_VAR_INIT`  |
| `src/bytecode/exec_stackless.rs` | `PUT_VAR_STRICT`：同上，外加 bool 检查                                                                                                                                                                              | `PUT_VAR_STRICT`           |
| `src/bytecode/exec_stackless.rs` | `MAKE_VAR_REF`：保持 ATOM 不变                                                                                                                                                                                      | `MAKE_VAR_REF`             |

**验证**：全局变量读写、严格模式变量、test262 `language/expressions/assignment`。

#### P3.7 Pass 2 死代码消除：已对齐，无需改动

> **审校纠正**（2026-07-06）：经二次确认，QuickJS 的 `resolve_variables`（Pass 2）中同样对 `OP_goto | OP_tail_call | OP_tail_call_method | OP_return | OP_return_undef | OP_throw | OP_throw_error | OP_ret` 调用 `skip_dead_code`（[quickjs.c#L34243-L34267](file:///Users/johnray/devel/quickjs-2026-06-04/quickjs.c#L34243-L34267)），与 Zhuque 的 [resolve_vars.rs#L290-L320](file:///Users/johnray/devel/zhuque/src/function/resolve_vars.rs#L290-L320) 完全一致。二者 Pass 2 在死代码消除上的行为已对齐，**无需任何改动**。原 P3.7 提议基于错误假设，已废弃。

> **✅ P3 已完成**（2026-07-21）。所有子项（P3.1 opcode 定义 → P3.2 数据结构 → P3.4 resolve 输出 → P3.5 初始化 → P3.6 运行时解释器）均已实现：`get_var`/`get_var_undef`/`put_var`/`put_var_init` 改为 3B `VAR_REF` 格式，`exec_stackless.rs` 通过 `captured_vars.split_at_mut(global_var_count)` 分离全局变量 VarRef。

---

### P4：Parser 配套改动 ✅ P4.1 ✅ P4.3 ✅ 已完成

**目的**：使用 P2 新增的 opcode，使派生类构造函数、async 迭代等路径对齐 QuickJS。

#### P4.1 派生类构造函数 ✅ 已完成

| 文件                             | 修改内容                                                                            | 涉及 opcode                       |
| -------------------------------- | ----------------------------------------------------------------------------------- | --------------------------------- |
| `src/parse/class.rs`             | `super()` 调用后 emit `INIT_CTOR` + `SCOPE_PUT_VAR_INIT(this)`                      | `INIT_CTOR`, `SCOPE_PUT_VAR_INIT` |
| `src/bytecode/exec_stackless.rs` | `INIT_CTOR` handler：实现 `super()` 调用逻辑（`GetPrototype` → `CallConstructor2`） | `INIT_CTOR`                       |

**实现细节**：

- 默认构造函数：`emit_class_default_ctor` 中 `byte_code.clear()` 后 emit `INIT_CTOR` + `SCOPE_PUT_VAR_INIT(this)` + `emit_class_field_init` + `SCOPE_GET_VAR_CHECKTHIS(this)` + `RETURN`，与 QuickJS `emit_return(s, FALSE)` 生成的字节码完全一致
- 显式 `super()`：Parser 中 `FUNC_CALL_SUPER_CTOR` 路径 emit `GET_SUPER` + `CALL_CONSTRUCTOR` + `DUP` + `SCOPE_PUT_VAR_INIT(this)` + `emit_class_field_init`，与 QuickJS 的 `js_parse_function_decl` 中 `FUNC_CALL_SUPER_CTOR` 路径完全一致
- `INIT_CTOR` handler：通过 `GetPrototype` → `call_internal(UNDEFINED, new_target, args, JS_CALL_FLAG_CONSTRUCTOR)` 实现，等价于 QuickJS 的 `JS_CallConstructor2(ctx, super, new_target, argc, argv)`

**验证**：`class extends` 相关 test262 测试

#### P4.2 set_loc_check 支持 ✅ 已完成

| 文件                                  | 修改内容                                                                                            | 涉及 opcode     |
| ------------------------------------- | --------------------------------------------------------------------------------------------------- | --------------- |
| `src/function/resolve_scoped_vars.rs` | `scope_put_var` → lexical local 且 KEEP_TOP（赋值表达式结果）时 emit `SET_LOC_CHECK` 替代 `PUT_LOC` | `SET_LOC_CHECK` |
| `src/bytecode/exec_stackless.rs`      | `SET_LOC_CHECK` handler：set + check initialized                                                    | `SET_LOC_CHECK` |

**验证**：词法变量复合赋值（如 `let x; x += 1`）的字节码确认 emit `SET_LOC_CHECK`，test262 回归

#### P4.3 for_await_of_next 支持 ✅ 已完成

| 文件                             | 修改内容                                                      | 涉及 opcode                        |
| -------------------------------- | ------------------------------------------------------------- | ---------------------------------- |
| `src/bytecode/exec_stackless.rs` | `FOR_AWAIT_OF_NEXT` handler：调用 `next()` + push promise     | `FOR_AWAIT_OF_NEXT`                |
| `src/bytecode/exec_stackless.rs` | 当前 `FOR_OF_NEXT` 中 async 路径拆出                          | `FOR_OF_NEXT`                      |
| `src/parse/statement.rs`         | for-await-of 循环 emit `FOR_AWAIT_OF_NEXT` 替代 `FOR_OF_NEXT` | `FOR_AWAIT_OF_NEXT`, `FOR_OF_NEXT` |

**实现细节**：

- `FOR_AWAIT_OF_NEXT` handler 调用 `next()` 前设置 `SP_PUT!(-1, JSValue::UNDEFINED)` 禁用 catch_offset，调用成功后立即恢复（P5.3 修复），防止异常时关闭（可能已损坏的）迭代器
- `FOR_OF_NEXT` 和 `FOR_AWAIT_OF_NEXT` 已拆分为独立的 handler
- Parser 中 async for-of 循环 emit `FOR_AWAIT_OF_NEXT` + `AWAIT` + `ITERATOR_GET_VALUE_DONE`，与 QuickJS `js_parse_for_in_of` 中 async 路径完全一致

**验证**：async iteration 测试

> **注**：`import` 的 parser 和运行时修正已在 P1.2 中完成，不重复列入 P4。

---

### P5：废弃 opcode 清理 ✅ 已完成

**目的**：P3 var_ref 迁移 + P4 parser 改完后，QuickJS 对齐路径已稳定。在最后阶段统一删除已不再使用的 opcode，杜绝两套体系并存。

**清理对象**：6 个不再发射的 **final** opcode（check_var/put_var_strict/define_var/check_define_var/define_func/with_get_ref_undef）。

#### P5.1 移除 build_opcodes.rs 中的定义 ✅ 已完成

| 移除 opcode          | 原定义             | 替代路径                                                                                                                                                                       |
| -------------------- | ------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `check_var`          | 5B, ATOM           | `GET_VAR_UNDEF` + uninitialized fallback                                                                                                                                       |
| `put_var_strict`     | 5B, ATOM           | `PUT_VAR` + runtime `is_strict_mode` 检查                                                                                                                                      |
| `define_var`         | 6B, ATOM_U8        | `ENTER_SCOPE` (**temp**) → resolve → `PUT_VAR_INIT` (**final**)                                                                                                                |
| `check_define_var`   | 6B, ATOM_U8        | 合并入 `ENTER_SCOPE` (**temp**) resolve 路径                                                                                                                                   |
| `define_func`        | 6B, ATOM_U8        | `FCLOSURE` + `PUT_VAR` (`PUT_LOC`)，均为已有 **final** opcode                                                                                                                  |
| `with_get_ref_undef` | 10B, ATOM_LABEL_U8 | 合并入 `with_get_ref`（QuickJS 中 `with_get_ref` 已内置 undef fallback，见 [quickjs.c#L20449-L20459](file:///Users/johnray/devel/quickjs-2026-06-04/quickjs.c#L20449-L20459)） |

#### P5.2 删除 resolve 和运行时引用 ✅ 已完成

| 文件                                  | 状态 | 修改内容                                                                                                         |
| ------------------------------------- | ---- | ---------------------------------------------------------------------------------------------------------------- |
| `src/function/resolve_scoped_vars.rs` | ✅   | 已移除 emit `check_var`/`define_var`/`check_define_var`/`define_func`/`put_var_strict` 的代码路径（P3 时已完成） |
| `src/function/resolve_vars.rs`        | ✅   | 不存在引用                                                                                                       |
| `src/bytecode/exec_stackless.rs`      | ✅   | 删除 CHECK_VAR、PUT_VAR_STRICT、WITH_GET_REF_UNDEF 三个 match arm                                                |
| `src/function/resolve_labels.rs`      | ✅   | 删除 `OP_with_get_ref_undef` 的 match arm + `PUT_VAR_STRICT` 优化匹配                                            |
| `src/function/stack.rs`               | ✅   | 删除 `OP_with_get_ref_undef` 的栈效应检查                                                                        |
| `src/function/optimize.rs`            | ✅   | 删除 `PUT_VAR_STRICT` 优化匹配                                                                                   |

#### P5.3 附加修复：FOR_AWAIT_OF_NEXT CatchOffset bug ✅ 已完成

在 P5 回归测试中发现 `for-await-of + 解构 + return` 触发的 `nip_catch` 内部错误。修复见 [exec_stackless.rs:2293-2303](file:///Users/johnray/devel/zhuque/src/bytecode/exec_stackless.rs#L2293-L2303)：

```rust
// FOR_AWAIT_OF_NEXT 在调用 next() 前禁用 catch_offset 防止异常时错误关闭迭代器，
// 调用成功后必须恢复 catch_offset，否则循环体内 return 触发的 NIP_CATCH 找不到标记。
let saved_catch = SP_VAL!(-1);  // 保存
SP_PUT!(-1, JSValue::UNDEFINED); // 禁用
// ... call next() ...
SP_PUT!(-1, saved_catch);  // 恢复 ← 关键修复
SP_PUT!(0, obj);
```

#### P5.4 验证 ✅ 已完成

- `cargo build` — 编译通过
- `cargo test` — 单元测试通过
- test262 回归 — 通过（含 `for-await-of + 解构 + return` 用例）

---

### 阶段依赖关系图

```
P1 (source_pos + import)       ← ✅ 已完成
    │
    ├──→ P2 (add opcodes)      ← ✅ 已完成
    │
    ├──→ P3 (var_ref 迁移)     ← ✅ 已完成
    │         └── P3.1~P3.6 (var_ref 迁移)
    │
    └──→ P4 (parser)           ← ✅ 已完成
         └── P4.1 (init_ctor)
         └── P4.2 (set_loc_check)
         └── P4.3 (for_await_of)
    │
    └──→ P5 (cleanup)          ← ✅ 已完成
                               (含 FOR_AWAIT_OF_NEXT CatchOffset 修复)
```

P1~P5 **全部已完成**。所有对齐目标均已实现。废弃的 6 个 final opcode 已从定义、运行时和 resolve 路径中移除。

### 保留不变的内容

| 内容                                              | 原因                                                                        |
| ------------------------------------------------- | --------------------------------------------------------------------------- |
| `GET_FIELD_IC` / `GET_FIELD2_IC` / `PUT_FIELD_IC` | Inline cache 是 Zhuque 独有的运行时优化，经 benchmark 验证有效（~15% 提升） |
| `last_line_num` 字段                              | ASI 自动分号插入等场景仍需要行号信息                                        |
| Byte ordering（Native endian）                    | x86_64 上 NE = LE，无实际差异                                               |
| `gc_alloc` 元信息字段                             | 仅为 Zhuque 调试/GC 优化服务                                                |
| short_opcode                                      | 本次不涉及                                                                  |
| 隐式 LINE_NUM 发射机制                            | 保留，仅改存储值                                                            |

### 对齐后可移除/降级的 opcode

经重新评估，以下 5 个 Zhuque 独有的 opcode 在字节码发射对齐 QuickJS 后**失去独立存在价值**，应标记为废弃：

#### `check_var` (5B, ATOM)

- **对齐前作用**：运行时检查全局变量是否存在，返回 bool
- **对齐后替代**：QuickJS 无此 opcode，存在性检查由 resolve 阶段完成（变量不存在则 fallback 到 realm_scope 全局对象查找），或隐含在 `GET_VAR_UNDEF` 的 uninitialized fallback 路径中
- **结论**：功能被 resolve 阶段 + `GET_VAR_UNDEF` 的运行时逻辑覆盖，**不再需要**

#### `put_var_strict` (5B, ATOM, n_pop=2)

- **对齐前作用**：strict 模式下 put_var，栈上额外有 bool（变量是否存在），需 pop 2 个值
- **QuickJS 方式**：在单一的 `PUT_VAR` handler 内通过 `is_strict_mode(ctx)` 运行时标志判断，零额外 opcode、零额外栈操作
- **对比**：QuickJS 的方案**更高效** — 少一个 opcode、少一次栈操作，判断逻辑统一
- **结论**：**冗余**，对齐后 `PUT_VAR` 内部整合 strict 检查即可

#### `define_var` (6B, ATOM_U8) / `check_define_var` (6B, ATOM_U8)

- **对齐前作用**：在作用域中定义变量（含检查）
- **QuickJS 方式**：变量定义通过 temp opcode `scope_put_var_init` 在 resolve 阶段完成，不产出额外的 final opcode。解析期写入 `ENTER_SCOPE` temp，resolve 时转为 `SET_LOC_UNINITIALIZED`（局部）或 `PUT_VAR_INIT`（全局）
- **对比**：当前 zhuque 将"检查+定义"拆成两个 6B final opcode，而 QuickJS 用 0 个额外 final opcode（全在 resolve 中消化）。QuickJS 的方式**既节省字节码体积又简化 interpreter**
- **结论**：**冗余**，resolve 阶段应统一由 `ENTER_SCOPE` + `PUT_VAR_INIT` / `SET_LOC_UNINITIALIZED` 路径处理

#### `define_func` (6B, ATOM_U8)

- **对齐前作用**：在作用域中定义函数名绑定
- **QuickJS 方式**：在 resolve 阶段 `ENTER_SCOPE` 处理中，函数声明类似 `FCLOSURE + PUT_VAR` 或 `FCLOSURE + PUT_LOC`。不需要专门的 define_func final opcode
- **结论**：**冗余**，`FCLOSURE` + `PUT_VAR`（或 `PUT_LOC`）即可表达

#### 处理方案（已执行 ✅）

| 阶段 | 操作                                                                   |
| ---- | ---------------------------------------------------------------------- |
| P5   | 从 `build_opcodes.rs` 中**移除**这 6 个 opcode 的 `DEF!` 定义          |
| P5   | resolve_scoped_vars.rs 中引用这些 opcode 的路径已改为 QuickJS 对齐路径 |
| P5   | exec_stackless.rs 中删除对应 match arm                                 |
| P5   | resolve_labels.rs/stack.rs/optimize.rs 中删除引用                      |

> **原则**：对齐后的字节码体系以 QuickJS 为标准，不在标准之外的 opcode 没有保留理由。保留两套语义等价但实现不同的路径，增加维护负担而无实际收益。

---

## 12. 字节码体系效率/密度/实现难度分析

### 12.1 操作码代码密度对比（核心操作）

| 操作                | QuickJS                               | Zhuque 当前                                                   | Zhuque P3 后              | 最优         |
| ------------------- | ------------------------------------- | ------------------------------------------------------------- | ------------------------- | ------------ |
| 全局 get var        | **3B** (get_var + u16)                | 5B (get_var + u32 atom)                                       | **3B** (var_ref)          | QuickJS / P3 |
| 全局 put var        | **3B** (put_var + u16)                | 5B (put_var + u32 atom)                                       | **3B** (var_ref)          | QuickJS / P3 |
| 属性 get            | 5B (get_field + u32 atom)             | 5B (get_field + u32 atom)                                     | 同                        | 持平         |
| 属性 get (IC)       | —                                     | 7B (get_field_ic + u32 + u16)                                 | 同                        | Zhuque 独有  |
| 局部 get var        | 3B (get_loc + u16)                    | 3B (get_loc + u16)                                            | 同                        | 持平         |
| push 小整数 (-1..7) | **1B** (push_0..7)                    | 5B (push_i32)                                                 | 5B                        | QuickJS      |
| push i8 范围        | **2B** (push_i8)                      | 5B                                                            | 5B                        | QuickJS      |
| push i16 范围       | **3B** (push_i16)                     | 5B                                                            | 5B                        | QuickJS      |
| call 0 参数         | **1B** (call0)                        | 3B (call + u16)                                               | 3B                        | QuickJS      |
| 短跳转 (<256B)      | **2B** (goto8)                        | 5B (goto + u32)                                               | 5B                        | QuickJS      |
| 本地变量读写        | 1~3B (get_loc0..3, get_loc8, get_loc) | 3B                                                            | 3B                        | QuickJS      |
| LINE_NUM            | 5B (1B + 4B offset)                   | **9B**（1B + 4B line + 4B col）；pc2line 列增量也对齐 QuickJS | Zhuque 多 4B 换 O(1) 列号 | 各有取舍     |
| ATOM_U8 复合        | **6B** (atom_u8)                      | **6B** (ATOM_U8)                                              | 同                        | 持平         |
| ATOM_U16 复合       | **7B** (atom_u16)                     | **7B** (ATOM_U16)                                             | 同                        | 持平         |

> **代码密度结论**：P3 对齐后全局变量访问从 5B 降到 3B（~40% 缩小），是本次改动最大的密度收益。短 opcode 若将来启用，整数和跳转指令还有 ~60-80% 的额外压缩空间。

### 12.2 运行时执行效率对比

| 操作           | QuickJS                                   | Zhuque 当前                                             | Zhuque P3 后                                   | 最优                   |
| -------------- | ----------------------------------------- | ------------------------------------------------------- | ---------------------------------------------- | ---------------------- |
| 全局 var 读    | `var_refs[idx]->pvalue`（1 次指针解引用） | `realm_scope.get_global_var(atom)`（scope/hash 查找）   | `captured_vars[idx].value()`（1 次指针解引用） | QuickJS / P3           |
| 全局 var 写    | `set_value(pvalue)`（直接写指针）         | `realm_scope.set_global_var(atom)`（scope 查找 + 写入） | `captured_vars[idx].set_value()`（直接写指针） | QuickJS / P3           |
| 属性读         | HashList 查找                             | IC fast path / HashList fallback                        | 同                                             | **Zhuque** (~15% 更快) |
| 属性写         | HashList 查找                             | IC fast path / HashList fallback                        | 同                                             | **Zhuque**             |
| 局部 var 读/写 | `frame->var_buf[idx]`（直接索引）         | `frame->var_buf[idx]`（直接索引）                       | 同                                             | 持平                   |
| 函数调度       | switch/case 跳表                          | 同等                                                    | 同                                             | 持平                   |

> **执行效率结论**：P3 的 var_ref 不仅减少字节码体积，还将全局变量访问从 scope 查找改为指针解引用，**同时提升了运行时性能**。Inline cache 则使 Zhuque 在属性访问上占优。

### 12.3 实现难度对比

| 特性              | QuickJS 路径                          | Zhuque 路径                                                                                   | 难度                    |
| ----------------- | ------------------------------------- | --------------------------------------------------------------------------------------------- | ----------------------- |
| 全局 var 间接寻址 | 新建 JSVarRef GC 节点系统             | **复用已有** `VarRef` + `captured_vars` + `ClosureVar`，全局 var 插入 `closure_var_refs` 头部 | Zhuque 更简（零新类型） |
| 短 opcode         | 两 pass resolve_labels peephole，复杂 | 已有 `push_vec_short_code`/`push_vec_short_int`，feature 开关                                 | 持平                    |
| 源码位置          | 345 处显式 `emit_source_pos` 调用     | 隐式 LINE_NUM 自动发射，零 parser 改动                                                        | **Zhuque 极简**         |
| Inline cache      | 无                                    | 解析期 emit + 运行时 IC 表                                                                    | Zhuque 独有             |
| 格式种类          | 12 种（单字符 FMT 宏）                | 22 种（build.rs 生成）                                                                        | 持平（自动生成）        |
| GC 分配元信息     | 无                                    | `gc_alloc: Never/Conditional/Always`                                                          | Zhuque 独有             |

### 12.4 最合理字节码体系评估

综合代码密度、执行效率、实现难度三个维度：

| 维度         | QuickJS 强项     | Zhuque 强项                          | 推荐                            |
| ------------ | ---------------- | ------------------------------------ | ------------------------------- |
| 变量访问密度 | var_ref 3B ★★★★★ | ATOM 5B ★★★                          | 采用 QuickJS var_ref（P3 实施） |
| 变量访问效率 | 指针解引用 ★★★★★ | scope 查找 ★★★                       | 采用 QuickJS 模式               |
| 属性访问效率 | ★★★              | IC ~15% 提速 ★★★★★                   | **保留 Zhuque IC**              |
| 整数压缩     | 1~3B ★★★★★       | 5B ★★                                | 将来 feature 启用               |
| 源码追踪     | 列级精度 ★★★★★   | 零 parser 改动 + O(1) 行列直读 ★★★★★ | **融合**：行列双值 + 隐式发射   |
| 格式丰富度   | 12 种            | 22 种                                | 保留 Zhuque                     |
| GC 优化信息  | 无               | gc_alloc 元信息                      | 保留 Zhuque                     |
| 实现简洁度   | 需多 pass        | 两 pass                              | 保留 Zhuque 两 pass             |

**最终结论**：

> 最合理的字节码体系是 **QuickJS 的 var_ref 寻址模型 + Zhuque 的 Inline Cache + 隐式 LINE_NUM（行列双值）+ Zhuque 的丰富格式与 GC 元信息**。
>
> 简记为：运行时代码路径取 QuickJS 之长（指针解引用替代 scope 查找），编译期基础设施留 Zhuque 之便（自动 LINE_NUM、IC 发射、格式丰富度）。
>
> 短 opcode 作为可选的密度优化，将来以 feature flag 独立追加。此次 P1~P4 不涉及。

---

## 13. 审校补充 — 排雷与遗漏修复

### 13.1 `put_var_strict` 的定位澄清

`put_var_strict` 是 Zhuque 独有的 opcode（QuickJS 没有独立定义，而是在 `OP_put_var` 中通过运行时的 `is_strict_mode` 标志区分行为）。因此 **不在对齐范围**，保持现有 ATOM 格式不变。在 P3 中，`put_var_strict` 既不被转换为 VAR_REF，也不改变运行时行为。

### 13.2 `last_line_pos` 维护方式

P1.1 最终采用 `last_line_pos`（列号）而非 `last_source_pos`（字节 offset）。`last_line_pos` 在 tokenizer 逐字符消费时增量维护：每读一个字节 `+1`，遇 `\n` 归零。与 `last_line_num` 维护方式完全对称。

> **注**：P1 修正后 `last_line_num` 和 `LINE_NUM` 路径保留，`last_line_pos` 作为 LINE_NUM 的第二个 u32 参数。两者联合去重（任一变化即 emit）。

### 13.3 闭合变量索引偏移：P3 引入的关键级联影响

P3 将全局变量 VarRef 插入 `closure_var_refs` 数组头部，导致**所有已有多余的闭合变量索引 + N**（`N = global_var_count`）。影响到的位置：

| 影响点                                    | 当前行为                                         | P3 后需调整为                       |
| ----------------------------------------- | ------------------------------------------------ | ----------------------------------- |
| `exec_stackless.rs` 中 `GET_VAR_REF idx`  | `captured_vars[idx]`（idx=0 对应第一个闭合变量） | `captured_vars[global_count + idx]` |
| `exec_stackless.rs` 中 `PUT_VAR_REF idx`  | 同上                                             | 同上                                |
| `exec_stackless.rs` 中 `CLOSE_LOC idx`    | 查 `captured_vars[idx]` 的 `is_match`            | 需偏移                              |
| `exec_stackless.rs` 中 `MAKE_LOC_REF idx` | `captured_vars[idx]`                             | 需偏移                              |
| `closure.rs` 中 `make_closure_for_arena`  | 构建 `closure_var_refs` 时按闭合变量数分配       | 需在前面留出 `global_count` 个槽    |

**推荐方案**：不修改所有 GET_VAR_REF/PUT_VAR_REF 等索引访问点，而是在运行时构建 `captured_vars` 时用**两个独立切片**：

```rust
// bfun.rs 或 exec setup：
let (global_var_refs, closure_var_refs) = captured_vars.split_at_mut(global_var_count);
// GET_VAR_REF idx → closure_var_refs[idx]（索引不变）
// GET_VAR idx → global_var_refs[idx]
```

或直接在 `BFuncPayload` 中新增独立字段 `global_var_refs`，与 `closure_var_refs` 分离，避免索引偏移影响全部现有代码。

> **建议**：优先采用分离字段方案（新增 `global_var_refs: Vec<VarRef>`），零影响已有闭合变量代码。

### 13.4 `pc2line` 映射表

P1 扩展 LINE_NUM 携带行列双值后，`pc2line` 编码已**对齐 QuickJS**（参考 [quickjs.c:34450-34499](file:///Users/johnray/devel/quickjs-2026-06-04/quickjs.c#L34450-L34499)）：

- **跳过条件**：`diff_line == 0 && diff_col == 0`（行列都没变才跳，与 QuickJS 一致）
- **列增量**：每个 entry 后都写 `SLEB128(diff_col)`，不分短编码/长编码（对齐 QuickJS 34494 行）
- **解码**：`find_line_num_col()` 每个 entry 后都读列增量（对齐 QuickJS 7482-7486 行）
- **编解码**：`push_vec_sleb128` 已改为标准 ZigZag 编码（`(val<<1)^(val>>31)`），与 `get_sleb128` 兼容
- **异常栈输出**：`filename:line:col` 格式（对齐 QuickJS 7556 行）

### 13.5 QuickJS 的 `check_define_var` 与 `define_var` 对比

在 QuickJS 中，没有独立的 `check_define_var` 和 `define_var` DEF opcode。变量定义和检查由 `scope_put_var_init` temp opcode 在 resolve 阶段完成（检查 + 定义逻辑内嵌）。Zhuque 的 `check_define_var` + `define_var` 是将 QuickJS 的一个 temp opcode 拆成了两个 final opcode。**此次对齐中两者保持 ATOM 不变**（不影响最终字节码兼容性），但可作为后续优化的候选：合并两个 opcode 为一个 temp，在 resolve 阶段完成。

### 13.6 `call_constructor` 和 `special_object` 在 9.4 表中的定位

第 9.4 节标题为"Stack Effect 语义不同"，但表中 `call_constructor` 和 `special_object` 两行实际标注为"语义相同/格式相同"。建议将其移至更合适的章节。**已确认**：两个 opcode 在 QuickJS 和 Zhuque 中完全一致，无差异，放在 9.4 仅用于完整性对照。

### 13.7 `with_get_ref_undef` — Zhuque 独有的 final opcode

Zhuque 的 [build_opcodes.rs#L196-L204](file:///Users/johnray/devel/zhuque/build_opcodes.rs#L196-L204) 中多出一个 **final** opcode（`DEF!`，非 temp）：

```rust
DEF!("with_get_ref_undef", 10, 1, 0, "ATOM_LABEL_U8", false, Never),
```

QuickJS 没有独立的 `with_get_ref_undef`，而是 `with_get_ref` 内部**已内置 undef fallback**（[quickjs.c#L20449-L20459](file:///Users/johnray/devel/quickjs-2026-06-04/quickjs.c#L20449-L20459)）：

```c
case OP_with_get_ref:
    ret = JS_HasProperty(ctx, obj, atom);
    if (!ret) {
        val = JS_UNDEFINED;   // 不存在则返回 undefined
    } else {
        val = JS_GetProperty(ctx, obj, atom);
    }
```

Zhuque 将这一逻辑拆分成了两个 final opcode：`with_get_ref`（抛异常）和 `with_get_ref_undef`（返回 undefined）。对齐 QuickJS 后，应将 undef fallback 逻辑合并入 `with_get_ref`，删除 `with_get_ref_undef`。

| 项目    | 数量                            |
| ------- | ------------------------------- |
| QuickJS | 1 个（`with_get_ref`）          |
| Zhuque  | 2 个（拆分为两个 final opcode） |
| 对齐后  | 1 个（合并回 `with_get_ref`）   |

**处理方案**：已在 P5 中删除 `with_get_ref_undef` 的 `DEF!` 定义及相关 runtime handler，与 5 个变量定义系列 opcode 一并移除。

---

## 14. `var_ref` (2B) vs `ATOM` (4B) 深度对比

### 14.1 编码面积

| 指标          | ATOM (4B atom_id)                    | VAR_REF (2B index)                       |
| ------------- | ------------------------------------ | ---------------------------------------- |
| 指令总大小    | 5B                                   | 3B                                       |
| 相对密度      | 基准                                 | **-40%（更紧凑）**                       |
| 16 位寻址空间 | N/A（atom 数量无限制）               | 65535 个全局变量/函数（充足）            |
| 序列化体积    | 每个引用的变量名在 atom 表中另有开销 | 变量名存入 `var_refs` 表，指令中仅为索引 |

### 14.2 一致性

| 维度                                        | ATOM                                                                                               | VAR_REF                                                               |
| ------------------------------------------- | -------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------- |
| 与 `get_field`/`put_field` 的关系           | 格式一致（均为 5B ATOM），统一但**语义混淆** — 变量访问和属性访问是不同的语义层次                  | 格式不同（3B vs 5B），但**语义精准** — 变量是词法绑定，属性是动态派遣 |
| 与 `get_loc`/`get_arg`/`get_var_ref` 的关系 | **不一致**（get_loc 3B/loc2B, get_arg 3B/arg2B, get_var_ref 3B/var_ref2B，唯独 get_var 5B/atom4B） | **一致** — 所有变量类 opcode 采用同构的 "opcode + 2B index" 模式      |
| Zhuque 体系内一致性                         | ❌ 变量访问家族中唯一使用 ATOM 的成员                                                              | ✅ 与已有的 6 个 2B-index 变量 opcode 对齐                            |

```
变量访问家族：
  局部变量:  get_loc    3B (1B opcode + 2B loc_idx)     ✅ 紧凑索引
  参数:      get_arg    3B (1B opcode + 2B arg_idx)     ✅ 紧凑索引
  闭包变量:  get_var_ref 3B (1B opcode + 2B var_ref_idx) ✅ 紧凑索引
  全局变量:  get_var    5B (1B opcode + 4B atom_id)     ❌ 唯一的异常
                   ↓ 对齐后
  全局变量:  get_var    3B (1B opcode + 2B var_ref_idx)  ✅ 统一完成
```

### 14.3 运行时效率

| 操作         | ATOM 路径                                                       | VAR_REF 路径                                                         |
| ------------ | --------------------------------------------------------------- | -------------------------------------------------------------------- |
| **读**       | `realm_scope.get_global_var(atom_id)` → scope chain / hash 查找 | `captured_vars[idx].value()` → 1 次指针解引用                        |
| **写**       | `realm_scope.set_global_var(atom_id, val)` → scope 查找 + 写入  | `captured_vars[idx].set_value(val)` → 1 次指针解引用 + GC 写屏障     |
| **首次访问** | 同后续                                                          | Uninitialized → fallback 到 realm_scope 查找（与 ATOM 路径同等开销） |
| **后续访问** | **同首次**（每次都要查）                                        | **命中缓存**（指针解引用，快一个数量级）                             |

**缓存效应**：高频全局变量（`console`、`Math`、`undefined`、模块导出）在 VAR_REF 下首次解析后实现零查找访问。ATOM 每次都是完整 scope 查找。

### 14.4 运行时内存

| 项目           | ATOM       | VAR_REF                                                                 |
| -------------- | ---------- | ----------------------------------------------------------------------- |
| 每函数固定开销 | 0          | `Vec<ClosureVar>`（编译期） + `Vec<VarRef>`（运行时 ~24B/变量）         |
| 每个引用       | 0          | 0（索引本身就是引用）                                                   |
| GC 压力        | 仅 atom 表 | VarRef 是 GC 节点（`GcRef<VarRefImpl>`），每个全局变量额外 1 个 GC 对象 |

> 评估：一个函数通常引用 3~10 个全局变量，额外 GC 开销约 72~240B/函数。与节省的字节码体积（每引用 2B × 平均引用次数）相比，对于中大型程序是高回报的。

### 14.5 调试与可读性

| 维度        | ATOM                        | VAR_REF                                         |
| ----------- | --------------------------- | ----------------------------------------------- |
| 字节码 dump | 直接可读：`GET_VAR console` | 需查表：`GET_VAR idx=3 → console`               |
| 工具链      | 无需外部数据                | dump 工具需持有 `var_refs` 表（QuickJS 已实现） |
| 人类调试    | 更友好                      | 稍不便（需多一次映射）                          |

### 14.6 综合评价

```
维度          ATOM    VAR_REF   胜者
───────────────────────────────────
代码密度       ★★      ★★★★★    VAR_REF (-40%)
运行时效率     ★★★     ★★★★★    VAR_REF (缓存命中)
体系一致性     ★★      ★★★★★    VAR_REF (对齐 get_loc 等)
语义精准度     ★★      ★★★★★    VAR_REF (变量≠属性)
调试友好度     ★★★★★   ★★★★     ATOM (略胜)
运行时内存     ★★★★★   ★★★★     ATOM (无额外 GC 节点)
扩展性         ★★★     ★★★★★    VAR_REF (可加缓存策略)
```

### 14.7 结论

> **VAR_REF 是更优方案，且不是 "牺牲一致性换密度"，而是 "修复了一致性缺陷"**。
>
> 当前 `get_var` 用 ATOM(5B) 是变量访问家族中唯一的异类——`get_loc`/`get_arg`/`get_var_ref` 全部用 2B 索引。切换到 VAR_REF 不是创造新不对称，而是**修正旧不对齐**。
>
> 同时 VAR_REF 带来运行时缓存效应（首次 scope 查找后转为指针解引用），对高频全局变量访问有显著加速。额外 GC 开销（每函数几十到几百 B）远小于字节码节省和运行时加速的综合收益。
>
> **Pass 数量不是瓶颈**——字节码扫描是 O(n) 的线性遍历，开销远小于源码解析（词法分析+语法分析），额外一个 pass 的代价可忽略。在编译器设计中，宁可多一个干净的 pass，也不要一个臃肿的多职责 pass。
