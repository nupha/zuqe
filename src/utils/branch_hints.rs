// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

#[inline(always)]
pub(crate) const fn unlikely(expr: bool) -> bool {
    if expr {
        #[cold]
        const fn cold_path() -> bool {
            true
        }
        cold_path()
    } else {
        false
    }
}

// #[inline(always)]
// pub(crate) const fn likely(expr: bool) -> bool {
//     if expr {
//         true
//     } else {
//         #[cold]
//         #[inline(never)]
//         const fn cold_path() -> bool {
//             false
//         }
//         cold_path()
//     }
// }

/// Xtensa架构（ESP32系列）的分支预测
///
/// ESP32内存布局
///   - IRAM (Instruction RAM): 128KB，快速访问
///   - DRAM (Data RAM): 320KB
///   - Flash: 外部，通过缓存访问，相对较慢
///
/// 优化策略
///  1. 热路径代码放在IRAM中
///  2. 冷路径代码可以留在Flash中
///  3. 关键的冷路径（如中断处理）放在IRAM中
#[cfg(target_arch = "xtensa")]
#[inline(always)]
pub fn unlikely_critical(expr: bool) -> bool {
    if expr {
        #[cold]
        #[inline(never)]
        #[link_section = ".iram1.text"] // 放入IRAM避免Flash缓存未命中
        fn xtensa_cold_path() -> bool {
            true
        }
        xtensa_cold_path()
    } else {
        false
    }
}
