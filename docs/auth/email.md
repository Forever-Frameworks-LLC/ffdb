# Transactional email and React Email templates

FFDB supports verification, password reset, email change, invitation, and a
future-compatible magic-link template. Each saved version records React source,
subject template, allowed variables, compiled HTML, plain text, compilation
errors, version, and the last successful compilation epoch.

## Compilation boundary

React/JavaScript source is untrusted developer input. It compiles only in an
isolated, resource-bounded build job with no provider credentials, project SQLite
file, control-plane database, or request-serving network access. Successful jobs
publish a versioned precompiled artifact. API request handling and email workers
never evaluate source or arbitrary JavaScript.

`@ffdb/email-components` contains safe, useful React Email defaults. The isolated
compiler may import that package and `@react-email/render`; the Rust runtime only
sees the output artifact.

## Runtime substitution

Runtime markers use `{{variable_name}}`. Variables must be declared scalars,
every declared value must be supplied, and unknown values fail closed. HTML
substitution escapes `&`, `<`, `>`, quotes, and apostrophes. Subject values reject
CR/LF header injection. Triple-brace/raw substitution and unsafe compiled markup
such as scripts, frames, forms, event handlers, or `javascript:` URLs are rejected.
Output size, mailbox length, variable count, and individual values are bounded.

Default variables are:

- `project_name`
- `action_url`
- `expires_in`

Preview rendering must use a restrictive CSP and a sandboxed frame. Preview HTML
is not proof that a source version compiled successfully; only the latest stored
successful artifact is eligible for delivery.

## Resend transport

Production delivery accepts only the exact `https://api.resend.com/` origin.
Redirects are disabled. DNS results are checked for private, loopback, link-local,
multicast, documentation, and unspecified networks, then pinned to the validated
addresses for the client lifetime to close rebinding races. A loopback HTTP origin
is allowed only through an explicit local-development exception.

Provider API keys are redacted from debug/log output and should be stored using
control-plane secret encryption. Each delivery carries a stable idempotency key.
Rate limiting, temporary provider failure, permanent rejection, and malformed
responses are distinct errors so the email queue can retry safely or dead-letter.
