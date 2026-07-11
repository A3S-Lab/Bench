---
name: a3s-box-pnpm-judge
description: Offline deterministic judge for the A3S Box pnpm benchmark.
tools: []
max_steps: 1
---

Evaluate dependency correctness and installation latency through this asset's
normal `judge.py:evaluate` entrypoint and `bench.judge.v1` capability. A3S OS
Runtime supplies protected inputs and a protected result channel with no network
or raw runtime secrets; the ordinary asset definition does not grant access to
private benchmark data.
