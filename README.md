# Kiln

Kiln is a Git-native agent orchestrator. It accepts `/agent` commands on pull request issue comments, validates policy, acknowledges accepted work with a PR comment plus a Check Run, and can hand accepted runs to a Kubernetes Job launcher.

## Current Scope

- `POST /webhooks/github` for GitHub webhook delivery.
- `GET /healthz` for health checks.
- GitHub webhook signature verification with `X-Hub-Signature-256`.
- `/agent` command parsing from line-start commands only.
- Opaque agent/model command metadata for the agent harness.
- Maintainer-level permission enforcement: `write`, `maintain`, or `admin`.
- Deterministic `kiln_<hash>` run IDs for idempotency.
- One Check Run per accepted command.
- Per-PR queue ordering for multiple commands in one comment.
- Optional `kubectl` Kubernetes Job launch mode.
- Runtime detection helpers for `.devcontainer/devcontainer.json` versus fallback images.
- Audit metadata helpers for future commit trailers.
- Recovery helpers for missing/stale run classification.
- Provider-neutral domain types for future provider support.
- Local HTTP fixture tests with a mocked GitHub client.

## Commands

Supported forms:

```text
/agent <task>
/agent:<agent> <task>
/agent:<agent>:<model> <task>
```

Bare `/agent <task>` leaves agent and model selection to the agent harness. Explicit agent/model values are preserved as opaque metadata, but Kiln does not validate them locally.

## Local Development

Local development does not require Docker, Kubernetes, a devcontainer, or a webhook tunnel for the primary workflow. Tests simulate signed GitHub webhook requests through the real Axum route and mock outbound GitHub API calls.

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

The default TOML config acknowledges commands without launching runtime jobs:

```toml
[server]
bind_address = "127.0.0.1:3000"

[execution]
mode = "disabled"
namespace = "default"
job_image = "ghcr.io/ixoo/kiln-agent:latest"
default_runtime_image = "ghcr.io/devcontainers/base:ubuntu"
```

Set `[execution].mode` to `kubectl` only in an environment where the Kiln process can run `kubectl apply -f -` against the target cluster. The generated Job receives run metadata, repository/PR metadata, command text, opaque agent/model values, and the configured fallback runtime image as environment variables.

The binary reads these environment variables:

- `KILN_ENV_FILE`: optional path to an env file to load before reading other settings.
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

Minimum tested GitHub App repository permissions:

- Checks: read and write.
- Issues: read and write.
- Pull requests: read and write.
- Metadata: read-only.

Subscribe the app to the `Issue comment` event. GitHub emits main pull request conversation comments as `issue_comment` events because pull requests are issues in GitHub's data model.

## Local GitHub Testing

A tunnel is only needed if you want GitHub.com to send webhooks to a process running on your laptop. The default workflow avoids that by using simulated HTTP tests.

See `docs/integration-testing.md` for the reusable manual GitHub App test setup.

## Current Boundaries

- The default execution mode is disabled; Kubernetes launch requires explicit config.
- Kiln generates runtime job metadata, but the agent harness container is not implemented in this repository yet.
- Kiln detects devcontainer intent in helper code, but does not run Devcontainer CLI itself.
- Model routing and agent/model validation belong to the agent harness.
- Database-backed state.
