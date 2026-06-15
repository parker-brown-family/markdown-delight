<!-- Thanks for contributing! Keep it focused — one change per PR. -->

## What & why

<!-- What does this change, and what problem does it solve? -->

## Type

- [ ] Theme (new/updated `.toml`)
- [ ] Bug fix
- [ ] Feature
- [ ] Docs / infra

## Checklist

- [ ] `cargo fmt -- --check`, `cargo clippy --locked -- -D warnings`, `cargo test --locked` all pass (in `app/`)
- [ ] `cargo deny check` passes (no new copyleft / GPL crates — the binary ships MIT)
- [ ] Screenshot included for anything user-visible
- [ ] For code touching the editor core: wrote it from `ropey` / `comrak` API docs, **not** Zed's GPL editor crate (clean-room rule, `docs/PLAN.md` §2)
