## What & why
<!-- What does this change and why. Link issues: Closes #123 -->

## Type
- [ ] fix  - [ ] feature  - [ ] docs  - [ ] refactor  - [ ] test  - [ ] chore

## Checklist
- [ ] `cargo fmt --all` + `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo test --workspace` green
- [ ] `flutter analyze` clean + `flutter test` green (if UI touched)
- [ ] New behaviour has tests
- [ ] Public APIs documented; affected `docs/` updated
- [ ] Respects the layering (domain depends on nothing; adapters implement ports)
- [ ] No new technical debt / duplicate logic / breaking API without a note

## Notes
<!-- Screenshots for UI, benchmarks for perf, migration notes for breaking changes -->
