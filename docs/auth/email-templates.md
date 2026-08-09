# Email Templates and Resend

Projects can version templates for verification, password reset, email change,
invitations, and future magic links. A version stores React Email source, subject,
compiled HTML, plain text, allowed variables, compilation errors, and last
successful compilation time.

Compilation runs only in a locked-down asynchronous build worker with CPU/memory/
time/output limits, no project secrets, no arbitrary network, and a read-only
runtime. The API never evaluates JavaScript during request handling. A failed
compile records safe diagnostics and does not replace the last good artifact.

Runtime delivery loads a validated precompiled artifact and substitutes exactly
the declared scalar variables. HTML is escaped, text is bounded, CR/LF is rejected
from headers, and unsafe HTML constructs/URL schemes are rejected. Preview uses the
same renderer and a restrictive iframe CSP.

Resend is deployment-scoped configuration in the current release. Its API key is
read from the process secret environment, must be injected by a production secret
manager, and is never returned by an API. Production delivery pins the exact
HTTPS Resend host, disables redirects, and rejects private/reserved DNS/IP
destinations. Jobs use stable idempotency keys, bounded exponential backoff,
attempt/audit records, and a dead-letter queue. Provider responses and logs never
include API keys or rendered secrets.
