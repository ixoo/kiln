# Agent Guidance

This repository is a new Rust service for a Git-native agent orchestrator.

## Current Focus

Implement Milestone 1 only unless the user explicitly expands scope:

- GitHub App webhook foundation.
- `/agent` command parsing.
- signature verification.
- maintainer permission checks.
- acknowledgement comments and Check Runs.
- local simulated HTTP tests.

Do not implement Kubernetes execution, devcontainer runtime support, model inference, commit pushing, or GitLab support yet.
Do not add Kiln-owned model or agent catalogs. Agent and model values in `/agent:<agent>:<model>` are opaque metadata until an agent harness integration can validate them.

## Engineering Rules

- Keep the orchestrator stateless. GitHub is the source of truth.
- Avoid databases, queues, workflow engines, and multi-agent orchestration.
- Prefer small, explicit Rust modules over broad abstractions.
- Keep GitHub API calls behind the `GitHubClient` trait so tests do not require network access.
- Use deterministic run IDs for idempotency.
- Preserve per-PR queue ordering for multiple commands.

## Verification

Before reporting changes as complete, run:

```sh
cargo fmt --check
cargo test
```

Prefer also running:

```sh
cargo clippy --all-targets -- -D warnings
```

Use mocked HTTP fixture tests for local validation. Do not require a real GitHub App for normal test runs.
Real GitHub App integration testing is documented in `docs/integration-testing.md`; keep all credentials and private keys out of git.

## CI and Releases

- GitHub Actions CI must build and test changes to `main` and pull requests targeting `main`.
- Release workflows are triggered by `v*` tags only.
- A release tag like `v0.1.0` must match the Cargo package version `0.1.0`; mismatches should fail the workflow.
- Release artifacts are Linux x86_64 only for now.
- Do not add Docker, devcontainer, cross-compilation, or release-distribution tooling unless explicitly requested.
