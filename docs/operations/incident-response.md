# Incident Response

## Priorities

Protect cross-project confidentiality and integrity first, then credentials and
availability. Preserve evidence without logging or copying additional secrets.
Every action is timestamped with an incident id and accountable actor.

## Initial response

1. Confirm the signal using request ids, audit records, metrics, and immutable
   provider logs. Do not run attacker-supplied SQL or download suspicious objects
   onto an operator workstation.
2. Classify scope: project(s), route generations, users/sessions, keys, workers,
   backups, storage keys/versions, sync cursors, and time window.
3. Contain with the narrowest safe control: suspend a project, fence a worker,
   revoke a key/session/family, disable signing, block a provider operation, or
   drain a release. Avoid destroying evidence.
4. Preserve relevant logs, audit rows, binaries/SBOM, database copies through the
   safe backup path, and provider versions under restricted access.
5. Eradicate the cause, add a failing regression test, deploy the reviewed fix,
   rotate affected credentials, and verify RLS/routing/storage/sync boundaries.
6. Recover from verified backups if required, require affected replicas to
   resnapshot, monitor for recurrence, and communicate impact/data-loss bounds.

## Scenario notes

- **Suspected cross-project/RLS leak:** suspend involved routes, fence workers,
  preserve database and audit state, invalidate sync scopes and signed URLs where
  possible, and treat all returned rows/objects as potentially disclosed.
- **Refresh/API/JWT key theft:** revoke token family/key, invalidate caches, rotate
  relevant signing/digest/envelope material, review audit use from first possible
  exposure, and notify affected developers/users.
- **Database corruption:** stop writes, preserve the original, restore a verified
  backup to a new path, and never attempt in-place repair on the only copy.
- **Provider compromise:** disable presigning/delivery, rotate provider credentials,
  reconcile object/message logs, and verify backup isolation.

A blameless post-incident review records root cause, failed controls, detection
latency, customer impact, recovery, regression tests, and concrete owners/dates.

