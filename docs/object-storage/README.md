# Object Storage and RLS

Object bytes live in an S3-compatible provider. Buckets, objects, uploads,
versions, owners, checksums, sizes, and provider-key mappings live in the project
SQLite database and use ordinary FFDB RLS policies:

```sql
CREATE POLICY users_read_own_objects ON storage_objects
  FOR SELECT TO authenticated
  USING (owner_id = auth.uid());
```

Every list/upload/download/delete/multipart action first executes the relevant
metadata read or mutation in the same RLS-secured session used for application
data. The provider adapter receives an opaque, authenticated, method/key/subject/
scope-bound authorization grant, never an unchecked caller path.

Object keys are normalized logical keys. Empty segments, dot segments, backslash,
control characters, path traversal, overlong keys, and reserved internal prefixes
are rejected before authorization. Provider keys are derived from trusted project
and metadata identifiers.

State-changing grants, including zero-byte delete and multipart actions, persist
a single-use reservation atomically with metadata authorization. Reservations are
bounded, expire and sweep independently, and abandoned multipart uploads are
cleaned by a durable job. Completion verifies declared size/checksum/content type
and consumes the exact reservation in the metadata transaction before making the
mutation visible. Downloads/listing/delete re-check RLS.

Signed requests use HTTPS outside explicit localhost development, bind method/key/
version/checksum/maximum bytes, have a short TTL, disable provider redirects, and
are redacted from logs. Provider endpoints use an explicit allowlist and DNS/IP
policy that rejects private, link-local, loopback, metadata, and rebinding targets.
