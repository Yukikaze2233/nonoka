# htmd autoresearch

This is an experiment to have the agent improve `htmd` performance automatically.

## Setup

To set up a new run, work with the user to:

1. Agree on the run branch. For this run, use `autoresearch-r1`.
2. Confirm the current branch is `autoresearch-r1`.
3. Read the in-scope files for context:
   - `README.md` for repository context and public expectations.
   - `benches/README.md` for the current benchmark target and prior reported numbers.
   - `benches/convert_bench.rs` for the benchmark harness. Do not change it unless the human explicitly asks.
   - `src/**/*.rs` for the implementation you are optimizing.
   - `tests/**/*.rs` for expected behavior.
4. Initialize `benches/results.tsv` with the header row if it does not exist. Keep it up to date as the experiment log for the branch.
5. Confirm setup looks good and begin experimentation immediately.

Once setup is complete, do not wait for further confirmation.

## Objective

The goal is simple: make `htmd` faster on the existing `convert()` benchmark without breaking correctness.

Primary metric:

- Lower runtime for `cargo bench --bench convert_bench`
- Track the Criterion `median` point estimate in milliseconds from `target/criterion/.../new/estimates.json`

Hard constraints:

- `cargo test` must pass before a change is considered valid.
- The benchmark target, benchmark input, and result interpretation stay fixed.
- Do not add new dependencies unless the human explicitly asks.

Soft constraints:

- Prefer changes that improve runtime without making the code much more complex.
- Small wins with simpler code are excellent.
- Performance gains that add brittle or ugly complexity are usually not worth keeping.

## What You Can Change

- Any implementation under `src/`
- Internal refactors that preserve the public API and behavior
- Allocation strategy, string building, traversal logic, data structures, and hot-path handling

## What You Should Not Change

- `benches/convert_bench.rs`
- The HTML benchmark fixture under `examples/page-to-markdown/html/`
- Tests, unless a test update is strictly required to reflect behavior the human requested
- `benches/results.tsv` is the canonical experiment log for the branch and may be committed alongside code changes

## Benchmark Commands

Use these commands:

1. Correctness check:

```bash
cargo test
```

2. Benchmark run:

```bash
./benches/bench_avg.sh convert_bench 3 run
```

3. Extract the benchmark result:

The helper prints output like:

```text
metric	median
warmup_runs	1
runs_ms	15.551	15.612	15.498
average_ms	15.554
stddev_ms	0.047
cv_pct	0.30
```

Treat `average_ms` as the tracked runtime in milliseconds. `runs_ms` now reflects the Criterion `median` point estimate for each run, not the console `time: [...]` interval.

If the helper output is missing, assume the run failed. Read the end of the last log:

```bash
tail -n 50 run3.log
```

## Logging Results

Log every experiment to `benches/results.tsv` as tab-separated values with this header:

```text
commit	runtime_ms	status	description
```

Columns:

1. Short git commit hash
2. Benchmark runtime in milliseconds using the helper's `average_ms` value
3. Status: `keep`, `discard`, or `crash`
4. Short description of the idea that was tried

Example:

```text
commit	runtime_ms	status	description
a1b2c3d	15.551	keep	baseline
b2c3d4e	14.982	keep	reduce intermediate string allocations in paragraph handling
c3d4e5f	15.890	discard	try more eager whitespace normalization
d4e5f6g	0.000	crash	switch shared buffer strategy, borrow checker bug
```

## Experiment Loop

The branch is dedicated to autonomous optimization.

LOOP FOREVER:

1. Check the current git commit and working tree.
2. Pick one performance idea grounded in the current code and benchmark target.
3. Edit the implementation directly.
4. Run `cargo test`. If tests fail, either fix the issue or discard the idea.
5. Run `./benches/bench_avg.sh convert_bench 3 run`.
6. Extract `average_ms` from the helper output.
7. Record that averaged runtime in `benches/results.tsv`.
8. If runtime improved, commit the experiment and advance from that new commit.
9. If runtime is equal or worse, do not commit it; revert the working tree to the starting point of that experiment.
10. Continue immediately with the next idea.

## Decision Rules

- The first experiment should always be the baseline with no code changes.
- Run the baseline benchmark 3 times and use the arithmetic average of the 3 helper-reported `median` point estimates as the baseline runtime.
- Compare later experiments against that averaged baseline, not a single baseline run.
- Never run multiple benchmark commands at the same time. Benchmark runs must be strictly sequential to avoid distorting results.
- Bench every passing experiment 3 times sequentially before any commit, and use the arithmetic average of those 3 runs for the decision.
- Commit only measured improvements. Do not create commits for flat or slower variants.
- Prefer measured improvements over speculative ones.
- Preserve correctness over speed.
- Be skeptical of noisy wins. If a result is close to flat, rerun or discard.
- Use judgment on benchmark variance, but do not keep regressions.
- If a crash is caused by a simple mistake, fix it and rerun once or twice. If the idea is fundamentally bad, log it as `crash` and move on.

## Autonomy

Once the loop begins, do not stop to ask the human whether you should continue. Do not ask whether to keep going. Continue autonomously until the human interrupts you.

If you run out of ideas, read the hot paths again, inspect allocations, reduce cloning and temporary strings, simplify control flow, and try more ambitious structural changes. Keep searching for the next measured improvement.
