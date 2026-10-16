#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>
#
# Reproducible M2b-6 benchmark matrix: build the release example and run every
# kernel under interpreter / tier1 / tier2 / automatic in one command.
# The tier2 column needs epic F1/F2 (type feedback + int specialization).
#
# Extra arguments go to the example, so the epic G4 protocol is one word away:
#   scripts/jit-bench-matrix.sh --protocol process --rounds 30
# (fresh interleaved processes + bootstrap confidence intervals; slower).
set -eu
cd "$(dirname "$0")/.."

cargo build --release --features jit --example jit-bench
exec cargo run --release --features jit --example jit-bench -- \
    --modes interpreter,tier1,tier2,automatic "$@"
