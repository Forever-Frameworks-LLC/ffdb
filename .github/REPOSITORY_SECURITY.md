# Repository security controls

FFDB keeps workflow policy in the repository and applies GitHub-hosted controls
through `scripts/configure-github-security.sh`. The script is intentionally
idempotent: it can be rerun after workflow or repository changes.

## Apply the repository settings

Authenticate GitHub CLI as an organization owner with repository
administration access, then run:

```sh
gh auth login --hostname github.com
make github-security-apply
```

The apply target enables and verifies:

- secret scanning and push protection;
- dependency vulnerability alerts and Dependabot security updates;
- GitHub Actions restricted to GitHub-owned actions plus the exact third-party
  actions used by FFDB;
- full-SHA action pinning and a read-only default `GITHUB_TOKEN`;
- pull-request-only changes to `main`, strict required CI, linear history,
  resolved conversations, admin enforcement, and disabled force-push/deletion;
- immutable GitHub releases.

If the organization or enterprise already controls the selected Actions
allowlist, GitHub rejects repository-level replacement with HTTP 409. The apply
target treats that as inheritance, retains the higher-level policy, and verifies
that its effective allowlist contains every action FFDB requires. Additional
inherited entries cannot be removed at repository scope; full-SHA enforcement
still prevents tag-based action references in FFDB workflows.

The protected branch requires all seven CI jobs: `secret-scan`, `rust`,
`rust-arm64`, `fuzz`, `typescript`, `supply-chain`, and `compose`. The branch
requires a pull request but uses zero mandatory approvals so a single-owner
organization is not deadlocked. Increase the approval count and enable code
owner review after adding another trusted maintainer.

Run the read-only audit at any time:

```sh
make github-security-audit
```

Override the repository or branch only when testing a fork:

```sh
FFDB_GITHUB_REPOSITORY=owner/fork FFDB_GITHUB_DEFAULT_BRANCH=main \
  make github-security-audit
```

## Require organization 2FA

The script audits organization 2FA but does not enable it. GitHub requires an
owner confirmation because non-compliant outside collaborators can lose access.
Before applying the repository controls, verify every owner and collaborator has
2FA, then open:

`https://github.com/organizations/Forever-Frameworks-LLC/settings/security`

Under **Authentication security**, require two-factor authentication for the
organization. Prefer secure methods only after confirming every required user
has a passkey, security key, authenticator app, or GitHub Mobile configured.

## npm trusted publishing

The release workflow uses GitHub OIDC and npm provenance; it intentionally has
no `NPM_TOKEN`. In npm package settings, add a GitHub Actions trusted publisher
for each package below, using repository `Forever-Frameworks-LLC/ffdb` and
workflow filename `release.yml`:

- `@ffdb/client`
- `@ffdb/sync-client`
- `@ffdb/react`
- `@ffdb/react-native`
- `@ffdb/email-components`
- `@ffdb/cli`

After all six trusted publishers exist, delete any repository or organization
secret named `NPM_TOKEN`. Do not create an Actions environment restriction
unless the same environment name is also configured in every npm trusted
publisher entry and in the `publish-npm` workflow job.
