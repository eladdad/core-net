# AGENTS.md

## Purpose
This file gives coding agents a practical, repo-specific playbook for working in `core-net`.

## Project Snapshot
- Language: Rust (`edition = 2021`)
- Binary: `corenet` (`src/main.rs`)
- Domain: cross-host keyboard/mouse sharing over network
- Supported platforms in code: macOS and Windows (`src/input/macos.rs`, `src/input/windows.rs`)

## Build, Test, and Quality Checks
Run these from the repo root:

```bash
cargo build
cargo test
cargo fmt --all
cargo clippy -- -D warnings
```

Notes:
- CI runs build + tests on Ubuntu/macOS/Windows.
- CI runs `cargo fmt --all -- --check`.
- CI currently allows clippy warnings (`continue-on-error: true`), but agents should still fix warnings when touching relevant code.

## Architecture Map
- `src/main.rs`: CLI, command routing, server/client runtime loop glue.
- `src/input/`: platform input capture/injection abstractions and implementations.
- `src/network/`: TCP connection lifecycle, server/client event handling.
- `src/protocol/`: wire message types and codec framing/serialization.
- `src/screen/`: edge detection and screen layout logic.
- `src/config/`: TOML config schema, load/save, defaults.
- `src/discovery/`: mDNS browsing/advertising support.

## Working Rules for Agents
- Keep cross-platform abstractions in traits; keep OS-specific API calls behind `#[cfg(target_os = ...)]` boundaries.
- Prefer small, focused changes in the module that owns the behavior.
- Preserve protocol compatibility assumptions unless explicitly changing protocol version/handshake behavior.
- If you add or change a protocol message:
  - Update `src/protocol/message.rs`.
  - Ensure codec coverage still works in `src/protocol/codec.rs` tests.
  - Update send/receive handling in `src/network/` and `src/main.rs` as needed.
- If you change CLI behavior, update clap structs in `src/main.rs` and keep help text accurate.
- If you change configuration behavior, update defaults and serialization in `src/config/mod.rs` and keep generated sample config valid.
- Keep logging through `tracing` (avoid ad-hoc logging frameworks).

## Testing Expectations
- Add/adjust unit tests near changed logic (`#[cfg(test)]` blocks in module files).
- At minimum, run targeted tests for touched modules; prefer full `cargo test` when feasible.
- For behavior changes that affect user workflows, update `README.md` examples and flags.

## Safety and Scope Notes
- The README and some historical docs mention Linux, but active input implementations are macOS/Windows only. Do not claim Linux support unless you add it.
- Network and input paths are latency-sensitive; avoid unnecessary allocations in hot loops.
- Keep default behavior usable without external config (`Config::load_default()` fallback).
