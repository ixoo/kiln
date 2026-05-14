#!/usr/bin/env sh
set -eu

PATH="${PATH:-/usr/local/bin:/usr/bin:/bin}"
HOME="${HOME:-/tmp/kiln-opencode-home}"
export PATH HOME

repo_url="https://github.com/${KILN_REPOSITORY}.git"
worktree="/work/${KILN_RUN_ID}/repo"
output_file="/work/${KILN_RUN_ID}/opencode-output.md"

mkdir -p "$(dirname "$worktree")"

if [ "${GITHUB_TOKEN:-}" != "" ]; then
  askpass=$(mktemp)
  chmod 700 "$askpass"
  cat >"$askpass" <<'ASKPASS'
#!/usr/bin/env sh
case "$1" in
  *Username*) printf 'x-access-token' ;;
  *Password*) printf '%s' "$GITHUB_TOKEN" ;;
  *) printf '%s' "$GITHUB_TOKEN" ;;
esac
ASKPASS
  export GIT_ASKPASS="$askpass" GIT_TERMINAL_PROMPT=0
  trap 'rm -f "$askpass"' EXIT INT TERM
fi

git clone --quiet --no-tags --depth 1 "$repo_url" "$worktree"
git -C "$worktree" fetch --quiet --depth 1 origin "$KILN_HEAD_SHA"
git -C "$worktree" checkout --quiet --detach "$KILN_HEAD_SHA"

agent="${KILN_AGENT:-build}"
model="${KILN_MODEL:-${OPENCODE_MODEL:-}}"
if [ "$model" = "" ]; then
  printf 'KILN_MODEL or OPENCODE_MODEL must be set\n' >&2
  state="failed"
  detail="missing OpenCode model"
else
  set +e
  opencode run \
    --dir "$worktree" \
    --agent "$agent" \
    --model "$model" \
    --title "Kiln ${KILN_RUN_ID}" \
    "$KILN_TASK" >"$output_file" 2>&1
  status=$?
  set -e

  if [ "$status" -eq 0 ]; then
    state="completed"
    detail="OpenCode completed"
  else
    state="failed"
    detail="OpenCode exited with status ${status}"
  fi
fi

if [ -f "$output_file" ] && [ "${GITHUB_TOKEN:-}" != "" ]; then
  {
    printf 'Kiln OpenCode run `%s` %s.\n\n' "$KILN_RUN_ID" "$state"
    printf 'Command: `%s`\n\n' "$KILN_COMMAND"
    printf '<details><summary>OpenCode output</summary>\n\n```text\n'
    sed -e 's/```/` ` `/g' "$output_file"
    printf '\n```\n</details>\n'
  } >"${output_file}.comment"

  GH_TOKEN="$GITHUB_TOKEN" gh pr comment "$KILN_PR_NUMBER" \
    --repo "$KILN_REPOSITORY" \
    --body-file "${output_file}.comment"
fi

if [ "${KILN_CALLBACK_URL:-}" != "" ] && [ "${KILN_CALLBACK_TOKEN:-}" != "" ]; then
  escaped_detail=$(printf '%s' "$detail" | sed 's/\\/\\\\/g; s/"/\\"/g')
  curl -fsS \
    -X POST "$KILN_CALLBACK_URL" \
    -H "Content-Type: application/json" \
    -H "X-Kiln-Callback-Token: ${KILN_CALLBACK_TOKEN}" \
    --data "{\"run_id\":\"${KILN_RUN_ID}\",\"status\":\"${state}\",\"owner\":\"${KILN_REPOSITORY_OWNER}\",\"repo\":\"${KILN_REPOSITORY_NAME}\",\"repo_full_name\":\"${KILN_REPOSITORY}\",\"pr_number\":${KILN_PR_NUMBER},\"installation_id\":${KILN_GITHUB_INSTALLATION_ID},\"detail\":\"${escaped_detail}\"}"
fi

if [ "$state" = "completed" ]; then
  exit 0
fi

exit 1
