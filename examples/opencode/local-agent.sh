#!/usr/bin/env sh
set -eu

if [ "${1:-}" = "" ]; then
  printf 'usage: %s /path/to/opencode-agent.env\n' "$0" >&2
  exit 2
fi

# Kiln clears the inherited process environment in local mode. Source explicit
# agent credentials and paths from a file outside the repository.
# shellcheck disable=SC1090
. "$1"

PATH="${PATH:-/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin}"
HOME="${HOME:-/tmp/kiln-opencode-home}"
export PATH HOME

repo_url="https://github.com/${KILN_REPOSITORY}.git"
work_root="${KILN_WORK_ROOT:-/tmp/kiln-opencode-runs}"
worktree="${work_root}/${KILN_RUN_ID}/repo"
output_file="${work_root}/${KILN_RUN_ID}/opencode-output.md"

mkdir -p "$(dirname "$worktree")"
rm -rf "$worktree"

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
  exit 2
fi

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
else
  state="failed"
fi

{
  printf 'Kiln OpenCode run `%s` %s.\n\n' "$KILN_RUN_ID" "$state"
  printf 'Command: `%s`\n\n' "$KILN_COMMAND"
  printf '<details><summary>OpenCode output</summary>\n\n```text\n'
  sed -e 's/```/` ` `/g' "$output_file"
  printf '\n```\n</details>\n'
} >"${output_file}.comment"

if [ "${GITHUB_TOKEN:-}" != "" ]; then
  GH_TOKEN="$GITHUB_TOKEN" gh pr comment "$KILN_PR_NUMBER" \
    --repo "$KILN_REPOSITORY" \
    --body-file "${output_file}.comment"
fi

exit "$status"
