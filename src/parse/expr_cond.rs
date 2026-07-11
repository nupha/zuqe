// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

// The ternary / nullish-coalescing / logical-and-or handling that used to live
// here (parse_cond_expr / parse_coalesce_expr / parse_logical_and_or) has been
// folded into `parse_expr_binary` / `parse_higher_expr` in expr_unabin.rs, so
// that deeply parenthesised expressions no longer pay one native stack frame
// per wrapper function. This file is kept only as a placeholder so the
// `mod expr_cond;` declaration in the parse module can stay unchanged.
