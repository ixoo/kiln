# OpenCode Agent Examples

Kiln can launch any executable that understands the `KILN_*` environment variables. This guide provides working examples for [OpenCode](https://opencode.ai/) in local mode and Kubernetes mode.

The examples are intentionally comment-only. They run `opencode run` against the pull request head commit, post OpenCode's output as a PR comment, and let Kiln update the Check Run. They do not commit or push code.

## Command Format

Use a model-explicit command so the example does not depend on a default model:

```text
/agent:build:anthropic/claude-sonnet-4-5 review this PR for bugs and summarize the highest-risk findings
```

Kiln preserves `build` as `KILN_AGENT`, `anthropic/claude-sonnet-4-5` as `KILN_MODEL`, and the rest of the line as `KILN_TASK`. The wrapper scripts pass those values to:

```sh
opencode run --agent "$KILN_AGENT" --model "$KILN_MODEL" "$KILN_TASK"
```

## Prerequisites

For both modes, you need:

- A configured Kiln GitHub App. See `docs/integration-testing.md`.
- A GitHub token for the agent with access to clone the target repository and post PR comments.
- An OpenCode provider API key, for example `ANTHROPIC_API_KEY` for `anthropic/*` models.
- A test PR where your GitHub user has `write`, `maintain`, or `admin` repository permission.

The GitHub token is separate from Kiln's GitHub App private key. Kiln does not pass its app private key or installation token to agents.

## Local Mode

Local mode is the quickest way to run the example. It executes on the same host as Kiln and has no sandbox, so use it only on trusted machines and repositories.

1. Install local tools:

```sh
brew install gh git
brew install anomalyco/tap/opencode
```

Alternatively install OpenCode with:

```sh
npm install -g opencode-ai
```

2. Copy the example agent environment outside the repo:

```sh
mkdir -p "$HOME/.config/kiln-opencode"
cp examples/opencode/local.env.example "$HOME/.config/kiln-opencode/opencode-agent.env"
chmod 600 "$HOME/.config/kiln-opencode/opencode-agent.env"
```

3. Edit `$HOME/.config/kiln-opencode/opencode-agent.env`:

```sh
GITHUB_TOKEN=github_pat_replace_me
ANTHROPIC_API_KEY=sk-ant-replace-me
OPENCODE_MODEL=anthropic/claude-sonnet-4-5
PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin
HOME=/Users/<user>
KILN_WORK_ROOT=/tmp/kiln-opencode-runs
```

4. Copy the local Kiln config:

```sh
cp config/kiln.local-opencode.example.toml "$HOME/.config/kiln-opencode/kiln.toml"
```

5. Edit `local_command` in `$HOME/.config/kiln-opencode/kiln.toml` so the env file path is absolute:

```toml
[execution]
mode = "local"
local_command = ["/bin/sh", "examples/opencode/local-agent.sh", "/Users/<user>/.config/kiln-opencode/opencode-agent.env"]
launch_timeout_seconds = 1800
stale_run_seconds = 3600
```

6. Create a Kiln env file outside the repo:

```sh
KILN_GITHUB_WEBHOOK_SECRET=$(openssl rand -hex 32)
KILN_STATE_SECRET=$(openssl rand -hex 32)
KILN_AGENT_CALLBACK_SECRET=$(openssl rand -hex 32)
cat >"$HOME/.config/kiln-opencode/kiln.env" <<EOF
KILN_CONFIG=$HOME/.config/kiln-opencode/kiln.toml
KILN_GITHUB_APP_ID=<app-id>
KILN_GITHUB_WEBHOOK_SECRET=$KILN_GITHUB_WEBHOOK_SECRET
KILN_STATE_SECRET=$KILN_STATE_SECRET
KILN_PREVIOUS_STATE_SECRETS=
KILN_AGENT_CALLBACK_SECRET=$KILN_AGENT_CALLBACK_SECRET
KILN_GITHUB_PRIVATE_KEY_PATH=$HOME/.config/kiln-test/keys/kiln-test.private-key.pem
RUST_LOG=kiln=info
EOF
chmod 600 "$HOME/.config/kiln-opencode/kiln.env"
```

Use the same `KILN_GITHUB_WEBHOOK_SECRET` value in your GitHub App webhook settings.

7. Start Kiln:

```sh
KILN_ENV_FILE="$HOME/.config/kiln-opencode/kiln.env" cargo run
```

8. Expose Kiln to GitHub and update the GitHub App webhook URL:

```sh
cloudflared tunnel --url http://127.0.0.1:3000
```

GitHub App webhook URL:

```text
https://<generated-host>/webhooks/github
```

9. Comment on a PR:

```sh
gh pr comment <pr-number> --repo <owner>/<repo> --body "/agent:build:anthropic/claude-sonnet-4-5 review this PR for bugs"
```

Expected result:

- Kiln posts an acknowledgement comment.
- Kiln creates a queued Check Run, then marks it running/completed or failed.
- The local OpenCode wrapper posts a PR comment containing the OpenCode output.

## Kubernetes Mode

Kubernetes mode creates a Job through `kubectl apply -f -`. Kiln itself must run somewhere with `kubectl` configured for the target cluster.

The example uses a Kubernetes Secret for OpenCode and GitHub credentials. Kiln injects that Secret into each agent Job with `envFrom.secretRef`.

1. Build and publish the example image:

```sh
docker build -t ghcr.io/<owner>/kiln-opencode-agent:latest examples/opencode
docker push ghcr.io/<owner>/kiln-opencode-agent:latest
```

2. Create the agent Secret from the example:

```sh
cp examples/opencode/k8s-secret.example.yaml /tmp/kiln-opencode-secret.yaml
```

Edit `/tmp/kiln-opencode-secret.yaml` with real values:

```yaml
stringData:
  GITHUB_TOKEN: github_pat_replace_me
  ANTHROPIC_API_KEY: sk-ant-replace-me
  OPENCODE_MODEL: anthropic/claude-sonnet-4-5
```

Apply it:

```sh
kubectl apply -f /tmp/kiln-opencode-secret.yaml
```

3. Copy the Kubernetes Kiln config:

```sh
cp config/kiln.kubectl-opencode.example.toml "$HOME/.config/kiln-opencode/kiln.toml"
```

4. Edit the image and callback URL:

```toml
[execution]
mode = "kubectl"
namespace = "default"
job_image = "ghcr.io/<owner>/kiln-opencode-agent:latest"
job_env_from_secret = "kiln-opencode-agent"
callback_url = "https://<kiln-public-host>/callbacks/agent"
launch_timeout_seconds = 300
stale_run_seconds = 3600
```

5. Start Kiln with the same env-file pattern as local mode, then comment on a PR:

```sh
gh pr comment <pr-number> --repo <owner>/<repo> --body "/agent:build:anthropic/claude-sonnet-4-5 review this PR for bugs"
```

Expected result:

- Kiln creates a Kubernetes Job named from the run ID, for example `kiln-abc123...`.
- The Job clones the repository, checks out `KILN_HEAD_SHA`, and runs OpenCode.
- The Job posts an OpenCode output comment to the PR.
- The Job calls `POST /callbacks/agent`, and Kiln marks the Check Run completed or failed.

## Agent Environment

Kiln passes these variables to local commands and Kubernetes Jobs:

- `KILN_RUN_ID`
- `KILN_REPOSITORY_OWNER`
- `KILN_REPOSITORY_NAME`
- `KILN_REPOSITORY`
- `KILN_PR_NUMBER`
- `KILN_GITHUB_INSTALLATION_ID`
- `KILN_HEAD_SHA`
- `KILN_REQUESTER`
- `KILN_COMMAND`
- `KILN_TASK`
- `KILN_AGENT`, when specified in `/agent:<agent>`
- `KILN_MODEL`, when specified in `/agent:<agent>:<model>`
- `KILN_QUEUE_POSITION`
- `KILN_DEFAULT_RUNTIME_IMAGE`
- `KILN_CALLBACK_URL`, when configured
- `KILN_CALLBACK_TOKEN`, when callbacks are configured

## Troubleshooting

- If OpenCode cannot find a model, use `/agent:build:<provider/model> ...` or set `OPENCODE_MODEL` in the local env file or Kubernetes Secret.
- If cloning fails, verify `GITHUB_TOKEN` can read the target repository.
- If PR commenting fails, verify `GITHUB_TOKEN` can write issue or pull request comments.
- If Kubernetes Jobs launch but stay running forever, check that `callback_url` is reachable from the cluster and that `KILN_AGENT_CALLBACK_SECRET` is set on Kiln.
- If local mode cannot find `git`, `gh`, or `opencode`, set `PATH` correctly in `examples/opencode/local.env.example` after copying it.
