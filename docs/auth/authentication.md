# Authentication

End-user identities are project scoped. The initial provider supports normalized
email/password registration, email verification, sign-in, password reset/change,
session listing/revocation, account disabling, and rotating refresh tokens.

Passwords use versioned Argon2id PHC hashes with configurable memory/time/
parallelism. Verification is constant-time where applicable and triggers rehash
after a parameter upgrade. Passwords, hashes, and reset/verification tokens are
never logged.

Access tokens are short-lived Ed25519 JWTs. Refresh, verification, reset, and
developer API keys are random opaque values; persistence stores a lookup prefix
and keyed digest, never bearer plaintext. Refresh is an atomic rotation: the old
token becomes consumed as the new token is issued. Reuse of a consumed token
revokes the complete family and creates a high-signal audit event.

Registration, sign-in, reset, verification, and refresh have independent per-IP,
per-project, and per-identity rate limits. Enumeration-sensitive flows return a
generic accepted response. Email addresses are normalized consistently but the
original display form may be retained separately.

## Session lifecycle

Sign-in creates a refresh family/session and returns access/refresh tokens. Sign-
out revokes the session selected by the presented refresh token. Password change
always revokes every session, including the current session, atomically with the
new password hash. The `revoke_other_sessions` request field is retained for wire
compatibility but cannot weaken this invariant. Password reset and account
disabling also revoke every family. Session listing exposes safe device/IP
summaries and never token hashes; access tokens carry a signed session id so the
current session is identified without trusting client input. Every access-token
verification also checks that exact project/user session and its refresh family
are both live and unexpired, so session revocation and refresh-token reuse take
effect immediately rather than waiting for JWT expiry.

Project API keys with `auth:manage` may list safe account summaries and enable or
disable an account. Password hashes and bearer credentials are never serialized.
Disabling an account atomically revokes all of its refresh sessions.
