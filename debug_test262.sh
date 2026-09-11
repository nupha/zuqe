#!/bin/bash
echo "Args received: $@" >&2
cd /Users/johnray/devel/zhuque
exec ./target/debug/examples/test262 "$@" > test262.log 2>&1