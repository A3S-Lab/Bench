# A3S Box pnpm Example

This directory demonstrates an advanced performance task using the canonical
task layout and ACL ownership boundaries.

It is an intentionally incomplete authoring fixture, not yet a runnable or
published benchmark. Add the real public workspace, implement the placeholder
generic tool-Asset function and terminal hidden bundle, then run
`a3s bench advanced check` before locking. Publication also requires the
governance block described in the quick reference.

The single `private/judge` A3S Agent Asset declares the standard
`bench.judge.v1` capability and uses its normal entrypoint for the one terminal
evaluation. Hidden tests and measurement inputs stay outside the asset in
`private/bundle/`.

The shared A3S Asset schema and OS Runtime must support the declared generic
function entrypoint and protected result channel before this fixture can run;
Bench does not load the function itself.

The work environment receives only `public/`. A3S OS Runtime starts the locked
Judge AssetSnapshot with `role = judge`, mounts only the Runtime-derived
SubmissionSnapshot plus `private/bundle/` for that terminal evaluation, and
never mounts the full Candidate-private TerminalCheckpoint. None of `private/`
is mounted into Candidate work.
