# Object storage authorization

FFDB stores bytes in an S3-compatible provider and stores buckets, object rows,
multipart state, ownership, checksums, versions, and quotas in the project's
SQLite database. Storage has no independent ACL system. Every list, sign, upload,
download, multipart, mutation, and deletion starts with a metadata operation in
the same RLS-secured session used for application SQL.

New project databases bootstrap `storage_buckets`, `storage_objects`,
`storage_uploads`, and `storage_versions` as ordinary logical RLS surfaces. RLS is
enabled and default-deny until a project migration installs policies. Provider
object keys, multipart-provider mappings, reservations, and part records remain
protected internal tables and are never exposed by schema introspection.

## Authorization sequence

1. Normalize and validate the logical bucket/key. Absolute keys, NULs, `.` and
   `..` path segments, overlong keys, and reserved internal names fail before a
   provider call.
2. Evaluate the relevant `storage_buckets`, `storage_objects`,
   `storage_uploads`, or `storage_versions` read/write using the complete immutable
   authenticated request context. Upload, download, delete, create multipart,
   upload part, complete, and abort each use operation-specific RLS probes.
3. Resolve the provider key from trusted metadata. A caller never supplies a raw
   provider URL or provider key.
4. For every state-changing provider operation, generate a cryptographically
   random reservation id in trusted service code, then atomically re-check quota
   and persist the reservation in project SQLite. Zero-byte delete and multipart
   actions are reserved too, so they are single-use even though they consume no
   quota. Callers never choose reservation ids. The bounded process-local ledger
   is only admission control; the SQLite reservation is authoritative across nodes.
5. Mint an HMAC-authenticated, short-lived authorization grant bound to project,
   subject, scope fingerprint, method, bucket, provider key, checksum, byte limit,
   multipart id, server nonce, reserved bytes, and reservation-expiry generation.
6. The S3 adapter verifies that grant before asking the configured provider to
   produce a method/key-bound URL. Provider URL TTL is at most five minutes and
   never exceeds the remaining grant lifetime.
7. Re-authorize with a fresh complete verified auth context, consume the exact
   nonce/subject/token/bytes/expiry reservation, and record the resulting metadata
   mutation/version in one SQLite transaction. A later mutation failure rolls the
   consume back; a successful commit or explicit release is single-use. Old grants
   cannot consume a reused nonce from another project or reservation generation.
   Do not log the grant,
   signed URL, authorization header, or provider credentials.

`ffdb-object-storage` intentionally defines the provider and metadata-authorizer
contracts without returning a raw S3 client. Production presigners must disable
redirects, resolve the configured allowlisted hostname, reject private, loopback,
link-local, multicast, documentation, and unspecified addresses, and pin the
validated addresses to prevent DNS rebinding. Plain HTTP and loopback endpoints
are allowed only for an explicit local-development hostname exception.

Object listing is a logical SQLite metadata query with prefix and cursor
pagination, so every returned row is filtered by RLS. List cursors are HMAC
authenticated with a route-specific key and bound to project, subject, token,
role/claims scope, bucket, and prefix; tampering or replay under another operator
fails closed. The adapter deliberately denies broad provider `ListObjects`
authorization; provider listings are not an authorization source and can expose
keys that SQLite policy would hide.

## Policy example

```sql
ALTER TABLE storage_objects ENABLE ROW LEVEL SECURITY;

CREATE POLICY users_read_own_objects
ON storage_objects
FOR SELECT TO authenticated
USING (owner_id = auth.uid());

CREATE POLICY users_upload_own_objects
ON storage_objects
FOR INSERT TO authenticated
WITH CHECK (owner_id = auth.uid());
```

A signed URL remains a bearer capability until expiry. Keep TTLs short, bind the
HTTP method and exact key, use checksums and content-length conditions for writes,
and never include signed URLs in application logs or analytics.

## Quotas and multipart cleanup

The authorization decision supplies current project usage, project quota, and
maximum object size. `MetadataAuthorizer::reserve` closes the gap between parallel
presigns and later metadata commits with a SQLite transaction. Commit, release,
and cleanup are explicit adapter operations. Reservations are durably bound to
project, server nonce, subject, token, reserved bytes, and expiry. A release from
another subject, token, or project cannot consume the reservation. The local
ledger has a configured entry cap and sweeps expired entries before new
allocations; SQLite remains authoritative across processes and nodes.

Multipart part metadata is bound to the logical upload id and part number before
commit. Abandoned multipart uploads should be aborted after their metadata lease
expires; successful completion validates the final checksum and committed byte
count before moving reserved bytes to used bytes. Developer-only worker operations
create and enumerate bucket metadata, validate UUIDs, S3-safe logical names, and
quota relationships, and still use trusted parameterized SQLite statements.
