# Report cases

Each folder is a wrong answer codemap once gave, turned into a test:
`project/` is the made-up example (neutral names, none of anyone's real code)
and `case.json` the call and the check of the right answer. The test
`crates/codemap-cli/tests/report_cases.rs` indexes every `project/` and runs
its check, so a fixed bug cannot come back.

Add one with `codemap reports --to-fixture <report id>` in the project that
has the report (it must have a made-up example that reproduces).
