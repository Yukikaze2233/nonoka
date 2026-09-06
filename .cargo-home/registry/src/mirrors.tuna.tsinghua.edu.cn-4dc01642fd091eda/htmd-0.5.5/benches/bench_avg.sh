#!/usr/bin/env bash
set -euo pipefail

bench_name="${1:-convert_bench}"
count="${2:-3}"
prefix="${3:-run}"
metric="${HTMD_BENCH_METRIC:-median}"
warmup_runs="${HTMD_BENCH_WARMUP_RUNS:-1}"

if ! [[ "$count" =~ ^[0-9]+$ ]] || [ "$count" -le 0 ]; then
  echo "count must be a positive integer" >&2
  exit 1
fi

if ! [[ "$warmup_runs" =~ ^[0-9]+$ ]]; then
  echo "HTMD_BENCH_WARMUP_RUNS must be a non-negative integer" >&2
  exit 1
fi

case "$metric" in
  mean|median) ;;
  *)
    echo "HTMD_BENCH_METRIC must be 'mean' or 'median'" >&2
    exit 1
    ;;
esac

for i in $(seq 1 "$warmup_runs"); do
  cargo bench --bench "$bench_name" > /dev/null 2>&1
done

for i in $(seq 1 "$count"); do
  log_file="${prefix}${i}.log"
  cargo bench --bench "$bench_name" > "$log_file" 2>&1
  estimates_path=$(python3 - "$log_file" <<'PY'
import json
import pathlib
import re
import sys

log_path = pathlib.Path(sys.argv[1])
log_text = log_path.read_text()
match = re.search(r"^Benchmarking (.+?)(?::|$)", log_text, re.MULTILINE)
if not match:
    sys.exit(1)

full_id = match.group(1).strip()
for benchmark_json in pathlib.Path("target/criterion").glob("*/new/benchmark.json"):
    data = json.loads(benchmark_json.read_text())
    if data.get("full_id") == full_id:
        print(benchmark_json.with_name("estimates.json"))
        break
else:
    sys.exit(1)
PY
)
  if [ -z "$estimates_path" ]; then
    echo "missing Criterion estimates.json output after run $i" >&2
    exit 1
  fi
  cp "$estimates_path" "${prefix}${i}.estimates.json"
done

python3 - "$count" "$prefix" "$metric" "$warmup_runs" <<'PY'
import json
import math
import pathlib
import sys

count = int(sys.argv[1])
prefix = sys.argv[2]
metric = sys.argv[3]
warmed = int(sys.argv[4])

def read_metric(run_idx: int) -> float:
    estimates_path = pathlib.Path(f"{prefix}{run_idx}.estimates.json")
    if not estimates_path.exists():
        print(f"missing estimates.json after run {run_idx}", file=sys.stderr)
        sys.exit(1)
    estimates = json.loads(estimates_path.read_text())
    selected = estimates.get(metric, {})
    point_estimate = selected.get("point_estimate")
    if point_estimate is None:
        point_estimate = estimates["mean"]["point_estimate"]
    return point_estimate / 1_000_000.0

values = []
for i in range(1, count + 1):
    log_path = pathlib.Path(f"{prefix}{i}.log")
    if not log_path.exists():
        print(f"missing benchmark result in {log_path}", file=sys.stderr)
        sys.exit(1)
    values.append(read_metric(i))

average = sum(values) / len(values)
variance = sum((value - average) ** 2 for value in values) / len(values)
stddev = math.sqrt(variance)
cv = 0.0 if average == 0 else (stddev / average) * 100.0

print(f"metric\t{metric}")
print(f"warmup_runs\t{warmed}")
print("runs_ms\t" + "\t".join(f"{value:.3f}" for value in values))
print(f"average_ms\t{average:.3f}")
print(f"stddev_ms\t{stddev:.3f}")
print(f"cv_pct\t{cv:.2f}")
PY
