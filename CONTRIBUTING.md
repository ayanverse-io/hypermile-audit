# Contributing to hypermile-audit

Thanks for helping. This CLI is intentionally small and local-only.

## Ground rules

- **No network I/O in the audit path.** Discovery, parse, and report must stay offline.
- **No telemetry.** Do not add crash reporters, analytics, or phone-home calls.
- **No prompt/file contents in output.** Reports may show paths, tool names, and aggregates only.
- Keep the public `--json` schema (`"schema": 1`) stable; additive fields need a schema bump discussion.

## Setup

```
cargo test
cargo build --release
```

## Pull requests

1. Add/adjust fixtures under `tests/fixtures/` when behavior changes.
2. Keep `unsafe_code = "forbid"`.
3. Run `cargo test` and `cargo fmt` before opening a PR.
