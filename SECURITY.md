# Security Policy

FFDB is pre-release. Report suspected vulnerabilities privately to the repository
maintainers and include affected version, reproduction, impact, and suggested
mitigation. Do not include credentials or production data.

Security invariants and the formal model live in
`docs/threat-model/threat-model.md`. Changes touching authentication, routing,
SQLite authorization, RLS compilation, storage presigning, synchronization,
secret handling, backups, or provider endpoints require adversarial regression
tests and security-owner review.

Supported releases will be listed here after the first signed release. Until then,
only the current main branch receives fixes.
