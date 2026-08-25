#!/usr/bin/env bash
# Deterministic-test discipline: the binding layer must never use real time.
# All timestamps flow through swr-core's Runtime (GpuiRuntime -> executor
# clock, virtual under #[gpui::test]); real sleeps/instants cause flaky tests.
set -euo pipefail
cd "$(dirname "$0")/.."

violations=$(grep -rn --include='*.rs' \
  -e 'std::thread::sleep' \
  -e 'Instant::now()' \
  crates/gpui-query/src crates/gpui-query/tests 2>/dev/null || true)

if [ -n "$violations" ]; then
  echo "Real-time API usage found (use the executor clock / timers instead):"
  echo "$violations"
  exit 1
fi
echo "time discipline OK"
