# Repository Guidelines

## Project Structure & Module Organization
- Rust workspace with a single binary crate `afc_finder`.
- Entry point: `src/main.rs` (egui/eframe UI, async via tokio).
- Configuration: `Cargo.toml` (dependencies like `idevice`, `egui`, `wgpu`).
- Assets: `icon.*`, `Assets.car`; keep platform assets in repo root.
- Tests: add unit tests inline (`#[cfg(test)]` in modules) and integration tests under `tests/`.

## Build, Test, and Development Commands
- Build debug: `cargo build` — compiles the app.
- Run with logs: `RUST_LOG=info cargo run` (PowerShell: `$env:RUST_LOG='info'; cargo run`).
- Build release: `cargo build --release` — optimized binary in `target/release/`.
- Lint: `cargo clippy -- -D warnings` — fail on warnings.
- Format: `cargo fmt --all` — apply rustfmt to the workspace.
- Tests: `cargo test` — run unit/integration tests.

## Coding Style & Naming Conventions
- Use rustfmt defaults (4‑space indent). Run `cargo fmt` before committing.
- Keep functions small; separate UI (egui) from device/IO logic for testability.
- Naming: `snake_case` for functions/modules, `CamelCase` for types/traits, `SCREAMING_SNAKE_CASE` for constants.
- Prefer `Result<T, E>` returns over panics in non‑UI code; log errors with `log`.

## Testing Guidelines
- Unit tests near logic they validate; mock device interactions behind traits where possible.
- Integration tests in `tests/` for end‑to‑end flows not requiring physical devices.
- Name tests descriptively (e.g., `afc_listing_sorts_by_name`).
- Ensure `cargo test` passes; avoid tests that require a connected device by default.

## Commit & Pull Request Guidelines
- Commits: short, imperative subject (≤72 chars), optional body explaining why.
- Reference issues (e.g., `Fixes #123`). Include screenshots/gifs for UI changes.
- PRs: clear description, reproduction steps, platform(s) tested (Windows/macOS/Linux), and risk/rollback notes.
- CI expectations: code is formatted, `clippy` clean, builds in release.

## Security & Configuration Tips
- Do not log sensitive device data or file contents.
- Validate user‑selected paths; never write outside the chosen directory.
- Keep async tasks non‑blocking; use channels already established in `main.rs`.

## Agent‑Specific Instructions
- Keep diffs minimal and focused; match existing structure and style.
- Prefer `rg` for search and `cargo` subcommands for validation.
- Avoid adding new dependencies unless justified and discussed in the PR.
