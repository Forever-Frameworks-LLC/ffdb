# JWT Claims

FFDB access tokens contain:

| Claim | Meaning |
| --- | --- |
| `iss` | configured FFDB issuer for the project |
| `aud` | configured project audience |
| `sub` | project-scoped end-user UUID |
| `role` | validated policy role, normally `authenticated` |
| `project_id` | opaque project UUID; trusted only after verification |
| custom claims | project-configured JSON scalars/objects within size limits |
| `iat`, `nbf`, `exp` | Unix epoch seconds with bounded clock skew |
| `jti` | unique token UUID for audit/revocation correlation |
| header `kid` | signing-key identifier used for safe rotation |

Verification chooses keys using the path project's trusted key metadata, then
validates algorithm, signature, issuer, audience, project, timestamps, subject,
role, and token id. The unverified `project_id`, `iss`, or `kid` never selects a
database route or arbitrary provider endpoint.

Inside SQLite policies, `auth.uid()` returns `sub`, `auth.role()` returns `role`,
`auth.jwt()` returns the immutable verified payload, and `auth.claim(name)` returns
one verified custom claim. User SQL cannot install or mutate this context.

