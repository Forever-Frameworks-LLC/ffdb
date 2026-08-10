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

## Email action handoff

Browser applications should provide an absolute `redirect_to` when registering
and a `redirectTo` action option when starting password recovery. FFDB places the
one-time credential and callback only in the email URL fragment, removes them
from the visible address bar before making the API request, and returns the
validated callback in the successful API response. The transition screen then
uses `location.replace()` so browser Back does not reopen the consumed link.

Callback URLs must use HTTP(S), contain no URL credentials, remain within the
bounded URL length, and exactly match an **Allowed auth redirect** saved for the
project under **Auth → Policy → Application URLs**. The adjacent **Allowed web
origins** list controls which browser origins may call that project's API. Both
lists are live project settings and require no SSH access or service restart.
The API checks the callback before consuming the action token and echoes only an
approved destination; the browser never redirects merely because a URL appeared
in an email fragment. If no callback was supplied, the transition screen gives
close-this-tab guidance instead of sending the user to the FFDB marketing page.

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
