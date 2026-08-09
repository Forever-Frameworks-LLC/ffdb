# Key and Credential Rotation

## JWT signing keys

1. Generate an Ed25519 key in the trusted key service. Encrypt the private key and
   create a new unique `kid` in `pending` state.
2. Publish its public key to verifiers and wait for cache propagation.
3. Mark it `active`; new tokens use it while the prior key remains `verifying`.
4. After the maximum access-token lifetime plus clock-skew/cache allowance, retire
   the prior key. Keep metadata for audit but erase private key material.
5. Test current and overlapping tokens, wrong-project/audience denial, and an
   unknown/retired `kid` before completing the audit event.

Emergency compromise skips the overlap for issuance, revokes the affected key,
revokes refresh families as risk requires, invalidates key caches, and follows the
incident runbook.

## Developer API keys

Create a replacement with the minimum scopes, update consumers, confirm use, then
revoke the old key. Keys are displayed once and stored only as a lookup prefix
plus keyed digest. Rotating the digest pepper requires a dual-read/single-write
window or explicit reissuance; never decrypt stored keys because no plaintext is
stored.

## Provider and envelope keys

Rotate PostgreSQL, S3, and Resend credentials using provider overlap where
available. Master-envelope key rotation decrypts and immediately re-encrypts each
secret under the new key with versioned authenticated encryption, records progress
idempotently, and retains the old key only until every record and backup policy is
accounted for. Cursor HMAC rotation uses a short multi-key verification window;
older cursors may safely require resnapshot.

