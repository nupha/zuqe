#!/bin/bash
# 包装器脚本，用于重定向输出并执行 test262 示例
# 使用 exec 将当前进程替换为 test262，这样调试器可以附加

cd "${WORKSPACE_FOLDER:-/Users/johnray/devel/zhuque}"

# 重定向 stdout 和 stderr 到文件
exec > test262.log 2>&1

# 执行实际的二进制文件
exec ./target/debug/examples/test262