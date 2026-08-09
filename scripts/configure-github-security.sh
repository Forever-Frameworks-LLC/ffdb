#!/bin/sh
set -eu

repository=${FFDB_GITHUB_REPOSITORY:-Forever-Frameworks-LLC/ffdb}
branch=${FFDB_GITHUB_DEFAULT_BRANCH:-main}
api_version=${FFDB_GITHUB_API_VERSION:-2026-03-10}
mode=audit

usage() {
  cat <<'EOF'
Usage: scripts/configure-github-security.sh [--audit|--apply] [options]

Options:
  --audit              Verify the expected repository controls (default)
  --apply              Apply repository controls, then audit them
  --repository OWNER/REPO
                       GitHub repository (default: Forever-Frameworks-LLC/ffdb)
  --branch NAME        Protected default branch (default: main)
  -h, --help           Show this help

The script intentionally does not enable organization 2FA. GitHub requires an
owner confirmation because enabling it can remove non-compliant outside
collaborators. The audit verifies that it has been enabled in the organization.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --audit)
      mode=audit
      ;;
    --apply)
      mode=apply
      ;;
    --repository)
      [ "$#" -ge 2 ] || { printf '%s\n' "--repository requires OWNER/REPO" >&2; exit 2; }
      repository=$2
      shift
      ;;
    --branch)
      [ "$#" -ge 2 ] || { printf '%s\n' "--branch requires a name" >&2; exit 2; }
      branch=$2
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'Unknown argument: %s\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

case "$repository" in
  */*) ;;
  *) printf '%s\n' "repository must be OWNER/REPO" >&2; exit 2 ;;
esac
case "$repository" in
  */*/*) printf '%s\n' "repository must contain exactly one slash" >&2; exit 2 ;;
esac
[ -n "$branch" ] || { printf '%s\n' "branch must not be empty" >&2; exit 2; }

command -v gh >/dev/null 2>&1 || {
  printf '%s\n' "GitHub CLI is required: https://cli.github.com/" >&2
  exit 2
}
gh auth status --hostname github.com >/dev/null 2>&1 || {
  printf '%s\n' "GitHub CLI is not authenticated. Run: gh auth login --hostname github.com" >&2
  exit 2
}

owner=${repository%%/*}
api_header="X-GitHub-Api-Version: $api_version"

put_json() {
  label=$1
  path=$2
  body=$3
  printf 'Applying: %s\n' "$label"
  printf '%s' "$body" | gh api --method PUT \
    -H 'Accept: application/vnd.github+json' -H "$api_header" \
    "$path" --input - >/dev/null
}

put_selected_actions() {
  path=$1
  body=$2
  printf '%s\n' "Applying: approved third-party Actions allowlist"
  if error_output=$(printf '%s' "$body" | gh api --method PUT \
      -H 'Accept: application/vnd.github+json' -H "$api_header" \
      "$path" --input - 2>&1 >/dev/null); then
    return
  fi
  case "$error_output" in
    *"organization or enterprise level"*)
      printf '%s\n' \
        "Inherited Actions allowlist detected; retaining the organization/enterprise policy."
      ;;
    *)
      printf '%s\n' "$error_output" >&2
      exit 1
      ;;
  esac
}

patch_json() {
  label=$1
  path=$2
  body=$3
  printf 'Applying: %s\n' "$label"
  printf '%s' "$body" | gh api --method PATCH \
    -H 'Accept: application/vnd.github+json' -H "$api_header" \
    "$path" --input - >/dev/null
}

if [ "$mode" = apply ]; then
  patch_json "secret scanning and push protection" "repos/$repository" \
    '{"security_and_analysis":{"secret_scanning":{"status":"enabled"},"secret_scanning_push_protection":{"status":"enabled"}}}'

  printf '%s\n' "Applying: dependency vulnerability alerts"
  gh api --method PUT -H 'Accept: application/vnd.github+json' -H "$api_header" \
    "repos/$repository/vulnerability-alerts" >/dev/null
  printf '%s\n' "Applying: Dependabot security updates"
  gh api --method PUT -H 'Accept: application/vnd.github+json' -H "$api_header" \
    "repos/$repository/automated-security-fixes" >/dev/null

  put_json "restricted GitHub Actions policy with SHA pinning" \
    "repos/$repository/actions/permissions" \
    '{"enabled":true,"allowed_actions":"selected","sha_pinning_required":true}'
  put_selected_actions \
    "repos/$repository/actions/permissions/selected-actions" \
    '{"github_owned_allowed":true,"verified_allowed":false,"patterns_allowed":["dtolnay/rust-toolchain@*","Swatinem/rust-cache@*","pnpm/action-setup@*","EmbarkStudios/cargo-deny-action@*","docker/setup-qemu-action@*","docker/setup-buildx-action@*","docker/login-action@*","docker/build-push-action@*","sigstore/cosign-installer@*"]}'
  put_json "read-only default GITHUB_TOKEN permissions" \
    "repos/$repository/actions/permissions/workflow" \
    '{"default_workflow_permissions":"read","can_approve_pull_request_reviews":false}'

  put_json "main branch pull-request and required-CI protection" \
    "repos/$repository/branches/$branch/protection" \
    '{"required_status_checks":{"strict":true,"contexts":["secret-scan","rust","rust-arm64","fuzz","typescript","supply-chain","compose"]},"enforce_admins":true,"required_pull_request_reviews":{"dismiss_stale_reviews":true,"require_code_owner_reviews":false,"required_approving_review_count":0,"require_last_push_approval":false},"restrictions":null,"required_linear_history":true,"allow_force_pushes":false,"allow_deletions":false,"block_creations":false,"required_conversation_resolution":true,"lock_branch":false,"allow_fork_syncing":true}'

  printf '%s\n' "Applying: immutable GitHub releases"
  gh api --method PUT -H 'Accept: application/vnd.github+json' -H "$api_header" \
    "repos/$repository/immutable-releases" >/dev/null
fi

audit_failed=0

expect_value() {
  label=$1
  path=$2
  expression=$3
  expected=$4
  if actual=$(gh api -H 'Accept: application/vnd.github+json' -H "$api_header" \
      "$path" --jq "$expression" 2>/dev/null); then
    if [ "$actual" = "$expected" ]; then
      printf 'OK: %s\n' "$label"
    else
      printf 'MISMATCH: %s (expected %s, got %s)\n' "$label" "$expected" "$actual" >&2
      audit_failed=1
    fi
  else
    printf 'MISSING: %s\n' "$label" >&2
    audit_failed=1
  fi
}

expect_endpoint() {
  label=$1
  path=$2
  if gh api -H 'Accept: application/vnd.github+json' -H "$api_header" \
      "$path" >/dev/null 2>&1; then
    printf 'OK: %s\n' "$label"
  else
    printf 'MISSING: %s\n' "$label" >&2
    audit_failed=1
  fi
}

expect_value "secret scanning" "repos/$repository" \
  '.security_and_analysis.secret_scanning.status' enabled
expect_value "secret scanning push protection" "repos/$repository" \
  '.security_and_analysis.secret_scanning_push_protection.status' enabled
expect_endpoint "dependency vulnerability alerts" "repos/$repository/vulnerability-alerts"
expect_value "Dependabot security updates" "repos/$repository/automated-security-fixes" \
  '.enabled' true
expect_value "selected Actions policy" "repos/$repository/actions/permissions" \
  '.allowed_actions' selected
expect_value "Actions SHA pinning" "repos/$repository/actions/permissions" \
  '.sha_pinning_required' true
expect_value "required third-party Actions allowlist" \
  "repos/$repository/actions/permissions/selected-actions" \
  '(.github_owned_allowed == true) and (.verified_allowed == false) and (.patterns_allowed as $actual | (["EmbarkStudios/cargo-deny-action@*","Swatinem/rust-cache@*","docker/build-push-action@*","docker/login-action@*","docker/setup-buildx-action@*","docker/setup-qemu-action@*","dtolnay/rust-toolchain@*","pnpm/action-setup@*","sigstore/cosign-installer@*"] - $actual | length == 0))' true
expect_value "default GITHUB_TOKEN is read-only" \
  "repos/$repository/actions/permissions/workflow" \
  '.default_workflow_permissions' read
expect_value "workflows cannot approve pull requests" \
  "repos/$repository/actions/permissions/workflow" \
  '.can_approve_pull_request_reviews' false
expect_value "strict required CI checks" "repos/$repository/branches/$branch/protection" \
  '[.required_status_checks.strict, (.required_status_checks.contexts | sort == ["compose","fuzz","rust","rust-arm64","secret-scan","supply-chain","typescript"]), .enforce_admins.enabled, .required_pull_request_reviews.dismiss_stale_reviews, (.required_pull_request_reviews.required_approving_review_count == 0), .required_linear_history.enabled, .required_conversation_resolution.enabled, (.allow_force_pushes.enabled | not), (.allow_deletions.enabled | not)] | all' true
expect_value "immutable releases" "repos/$repository/immutable-releases" '.enabled' true
expect_value "organization 2FA requirement" "orgs/$owner" \
  '.two_factor_requirement_enabled' true

if [ "$audit_failed" -ne 0 ]; then
  printf '\n%s\n' "GitHub hardening audit failed. See .github/REPOSITORY_SECURITY.md." >&2
  exit 1
fi

printf '\nGitHub repository hardening audit passed for %s (%s).\n' "$repository" "$branch"
