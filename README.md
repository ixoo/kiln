# Kiln

Kiln is a Git-native agent orchestrator. The first milestone is a GitHub App webhook service that accepts `/agent` commands on pull request issue comments, validates policy, and acknowledges accepted work with a PR comment plus a Check Run.

## Milestone 1 Scope

- `POST /webhooks/github` for GitHub webhook delivery.
- `GET /healthz` for health checks.
- GitHub webhook signature verification with `X-Hub-Signature-256`.
- `/agent` command parsing from line-start commands only.
- Opaque agent/model command metadata for the future agent harness.
- Maintainer-level permission enforcement: `write`, `maintain`, or `admin`.
- Deterministic `kiln_<hash>` run IDs for idempotency.
- One Check Run per accepted command.
- Per-PR queue ordering for multiple commands in one comment.
- Local HTTP fixture tests with a mocked GitHub client.

## Commands

Supported forms:

```text
/agent <task>
/agent:<agent> <task>
/agent:<agent>:<model> <task>
```

Bare `/agent <task>` leaves agent and model selection to the future agent harness. Explicit agent/model values are preserved as opaque metadata, but Kiln does not validate them locally.

## Local Development

Milestone 1 does not require Docker, a devcontainer, or a webhook tunnel for the primary development workflow. Tests simulate signed GitHub webhook requests through the real Axum route and mock outbound GitHub API calls.

Run tests:

```sh
cargo test
```

Run locally:

```sh
cp config/kiln.example.toml config/kiln.toml
cp .env.example .env
cargo run
```

The TOML config currently contains only the bind address:

```toml
[server]
bind_address = "127.0.0.1:3000"
```

The binary reads these environment variables:

- `KILN_CONFIG`: path to the TOML config file. Defaults to `config/kiln.toml`.
- `KILN_GITHUB_APP_ID`: numeric GitHub App ID.
- `KILN_GITHUB_WEBHOOK_SECRET`: webhook secret configured on the GitHub App.
- `KILN_GITHUB_PRIVATE_KEY_PATH`: path to the GitHub App private key PEM.
- `RUST_LOG`: optional tracing filter.

## Deployment

For real GitHub delivery, expose the service at a stable HTTPS URL and configure the GitHub App webhook URL to:

```text
https://<your-host>/webhooks/github
```

The service expects GitHub `issue_comment` events. Non-actionable events return `200` with an ignored reason so GitHub does not retry them.

## Local GitHub Testing

A tunnel is only needed if you want GitHub.com to send webhooks to a process running on your laptop. The default workflow avoids that by using simulated HTTP tests.

## Current Non-Goals

- Kubernetes Job execution.
- Devcontainer runtime execution.
- Model routing and inference.
- Commit-capable agent runtime.
- Database-backed state.

Those belong to later milestones in `plan.md`.
