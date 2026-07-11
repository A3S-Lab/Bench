---
name: smoke-judge
description: Offline evaluator for the a3s-bench smoke task.
tools: []
max_steps: 1
---

# Smoke judge

Evaluate the submitted `answer.txt` against the evaluator-owned expected value.
This standard Agent Asset declares the `bench.judge.v1` capability and uses its
normal `judge.py:evaluate` entrypoint. It must not request a model, network
access, credentials, or interactive tools while judging.
