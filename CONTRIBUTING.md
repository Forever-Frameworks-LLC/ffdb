# Contributing

Read the architecture, interface contracts, decision log, threat model, and the
nearest module documentation before editing. Security-boundary changes require an
adversarial test and review from the security owner.

Use focused commits and do not mix formatting or generated lockfile churn with an
unrelated behavior change. Public contract changes require a versioning decision
and corresponding SDK/API documentation.

Before opening a change, run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
pnpm check
pnpm test
pnpm build
docker compose config --quiet
```

For a security-sensitive bug, first add the smallest failing regression test,
then implement the fix without weakening authorizers, RLS, credential checks,
limits, isolation, or error redaction. Never commit real credentials, databases,
backups, signed URLs, `.env`, or production data.
