# Backup and Restore

Project backups use SQLite's online backup API from a worker-owned connection.
Copying a live database, WAL, or raw filesystem snapshot is not a supported
backup. PostgreSQL control-plane backups are separate and must be coordinated.

## Complete host recovery archives

The installed backup workflow creates one checksum-manifested recovery archive
without a repository checkout. This is the disaster-recovery path for the
packaged Docker `single-host` profile and the native Linux/systemd profile. It is
separate from a project-scoped encrypted backup: the host archive coordinates
PostgreSQL, every project SQLite database, the organization metrics/billing
ledgers, encrypted project-backup files, sync state, object metadata, and the
configuration required to recover that installation.

For a packaged single-host installation, choose a new absolute path on a
root-only filesystem:

```sh
sudo ffdb-host backup create /secure/ffdb-host-2026-08-03.tar.gz
```

`create` verifies that the API and PostgreSQL are running, quiesces the gateway
and mutation-serving services, creates a PostgreSQL custom dump, archives all
six application/provider data volumes, records the exact FFDB version and
profile, writes SHA-256 checksums, publishes the final archive at mode `0600`,
and resumes the complete stack. It never overwrites an existing path. Its
failure trap also attempts to resume the stack; if resumption fails, the command
exits unsuccessfully and reports that condition.

Restore must use the exact FFDB version that created the archive. Stop the host
first and pass the explicit destructive confirmation:

```sh
sudo ffdb-host stop
sudo ffdb-host backup restore /secure/ffdb-host-2026-08-03.tar.gz --yes
```

The restore refuses a running host. Before deleting or replacing anything, it
rejects traversal, links and special files, verifies the manifest/profile/exact
version, verifies every file checksum and rejects unverified files, validates
the PostgreSQL dump, extracts every nested volume into staging, runs SQLite
`quick_check` over project and organization-metrics databases, and validates the
archived Compose configuration. Only then does it replace the named volumes and
root-only environment file. PostgreSQL restores in one transaction; ownership
is restored from the volume archives; SQLite integrity is checked again from the
restored volumes; and success requires the compiled gateway readiness endpoint.

The single-host archive includes PostgreSQL, MinIO object bytes and metadata,
Mailpit state, project data, encrypted backups, organization metrics, sync state,
and generated configuration. It therefore contains both secrets and customer
data. The mode-`0600` local archive is only the first control: encrypt it with an
independently managed recovery key, replicate it off host, restrict access,
record its retention, and regularly restore it on an isolated host.

Native Linux bundles install `/usr/local/bin/ffdb-backup`. Native create stops
and later returns the API and sync worker to their original active/inactive
states; Caddy may remain up but cannot serve mutations while the API is stopped:

```sh
sudo ffdb-backup create /secure/ffdb-native-2026-08-03.tar.gz
sudo systemctl stop ffdb-sync-worker.service ffdb-api.service
sudo ffdb-backup restore /secure/ffdb-native-2026-08-03.tar.gz --yes
```

The native archive includes a logical dump of `FFDB_DATABASE_URL`, local project,
metrics, encrypted-backup and sync directories, object metadata held in those
databases, the exact installed version, and `/etc/ffdb/ffdb.env`. Restore first
validates all inputs, then reinstalls the environment as `root:ffdb` mode `0640`,
restores local state as `ffdb:ffdb` mode `0700`, restores PostgreSQL in one
transaction, starts the API and sync worker, and requires API readiness plus
gateway readiness when `ffdb-gateway.service` is active. Native production uses external object
storage, so the object provider's versioned bytes must be protected and restored
to the same recovery point; FFDB cannot copy an operator's external bucket.

The external-provider Compose profile likewise cannot make an atomic archive of
operator-owned PostgreSQL or S3. Use those providers' logical/snapshot and object
versioning workflows while FFDB ingress is quiesced. `ffdb-host backup` fails
closed on that profile instead of producing an incomplete archive.

## Encryption and key management

`FFDB_BACKUP_MASTER_KEY` is a required, base64-encoded secret that decodes to
exactly 32 bytes. It is independent from the platform envelope-encryption key and
must come from a production secret manager. The database worker derives a unique
AES-256-GCM key for every project, database, and backup UUID with HKDF-SHA-256 and
the `ffdb.backup.aead.v1` domain. Project, database, backup, chunk number, and final
chunk state are authenticated as associated data, so ciphertext cannot be moved
between routes or backup identifiers.

The worker encrypts in bounded 64 KiB chunks. The final
`BACKUP_UUID.sqlite3` file is ciphertext beginning with `FFDBBK01`, not a SQLite
database; its returned size and SHA-256 cover that ciphertext. The online backup
API requires a worker-created plaintext SQLite temporary file. Keep the backup
root on a trusted, access-controlled filesystem; the worker never accepts a path
from a request and removes plaintext and partial ciphertext temporary files after
success or failure. Operators must treat unexpected temporary files as sensitive
incident artifacts.

## Backup procedure

1. Authenticate a `backups:manage` developer key and create a backup job with a
   server-generated backup UUID.
2. The API acquires the project backup lifecycle lock and a fenced worker lease.
3. The worker runs the bounded online backup into a UUID-named trusted temporary
   file, performs SQLite's integrity check, encrypts it with authenticated chunked
   encryption, fsyncs it, and atomically publishes the ciphertext under the
   requested UUID.
4. The control plane records the local ciphertext identifier, size and SHA-256,
   actor, timestamps, and status. Temporary plaintext files are removed before a
   successful response is returned.
5. Replicate the encrypted backup root using the operator's durable volume/object
   backup system and alert if creation, integrity, encryption, replication, or
   retention cleanup fails.

Backups and their metadata must be access controlled independently from
application object buckets. They are excluded from end-user signed URL paths.
The FFDB job is complete when the encrypted file is atomically durable on the
configured backup volume; off-host replication and retention are explicit
operator responsibilities.

## Restore procedure

Restore is a privileged, audited, destructive lifecycle transition. The worker
accepts only a backup UUID and resolves it beneath the configured backup root.

1. Set project state to `restoring`, stop new work, drain active sessions, and
   fence old worker generations.
2. Ensure the expected encrypted UUID file is present on the trusted backup
   volume and verify the recorded ciphertext SHA-256 and size.
3. Send the UUID to the worker. It authenticates every ciphertext chunk before
   use; a wrong project/database/UUID, truncation, trailing data, or modification
   fails closed.
4. The worker creates an encrypted pre-restore recovery backup, decrypts to a new
   trusted temporary path, runs `PRAGMA quick_check` on the candidate, restores
   through SQLite's online backup API, refreshes protected RLS/sync structures,
   and verifies the live database after restore. A request-bound receipt marker
   is written into the candidate first and is therefore committed with the same
   SQLite backup transaction as the restored data.
5. On success the worker removes its recovery backup and returns the restored
   schema version. On an authenticated candidate or post-restore failure, it
   retains the encrypted recovery backup under a new UUID for operator recovery.
6. Invalidate prepared statements and incompatible sync scopes. Clients beyond
   the restored sequence must resnapshot. Run representative RLS, auth, storage,
   and sync probes before returning the project to `active`.
7. Record the restore result. A failed restore keeps its encrypted pre-restore
   recovery backup under a new UUID for explicit operator recovery.

If the API loses the worker response, the same idempotency key retains the
project's `restoring` fence and routes only that restore retry. The worker reads
the marker from the live database, reconstructs the original response, and does
not restore again; writes made after the first completed restore are preserved.

Operators must schedule a restore drill in an isolated project. A backup without
a successful recorded restore is not considered verified.

## Organization metrics and billing ledger

Project backups do not contain the organization usage ledger. FFDB keeps one
private SQLite database per organization below `FFDB_METRICS_ROOT`; packaged
Compose installations mount that path from the `metrics-data` named volume, and
native systemd installations use `/var/lib/ffdb/metrics` with mode `0700`. The
ledger contains idempotent read/write/storage/MAU usage, active reservations,
Stripe reporting outbox records, and reconciliation checkpoints. Losing it can
lose billable usage or admission history; restoring it to an unrelated point in
time can disagree with PostgreSQL subscription and reporting state.

The complete host workflow above is the primary coordinated path for packaged
single-host and native installations. If an external-provider deployment uses a
provider snapshot workflow, treat the metrics root as its own protected backup
set:

1. Block new ingress and drain or stop the API before taking a raw volume or
   filesystem copy. Do not copy live `.sqlite3`, `-wal`, or `-shm` files.
2. Capture the entire metrics root and PostgreSQL billing/control-plane state at
   one documented recovery point. Storage-native snapshots may be used only when
   both sides are quiescent and their ordering is recorded.
3. Encrypt the snapshot, record its digest and recovery timestamp, replicate it
   off host, and retain it according to financial and privacy policy. Hashed MAU
   identifiers remain sensitive billing data.
4. Restart FFDB, verify readiness, and confirm organization usage summaries and
   reporting health before reopening ingress.

For a metrics-ledger restore, keep ingress blocked, restore PostgreSQL and the
metrics root from the same recovery point, restore ownership and mode, then
validate every organization database with SQLite `quick_check` and reconcile all
four provider-meter dimensions before allowing billable writes. Never roll back
the metrics root merely because one project database is restored: the successful
operations recorded after that project snapshot remain part of the billing
history. A disaster-recovery runbook must explicitly account for any interval
between the selected ledger snapshot and the incident.

## Corruption response

Stop writes and preserve files and logs for analysis. Do not run repair tools
against the only copy. Restore the newest verified backup, replay durable logical
records only through validated worker operations, and compare integrity, schema,
and row counts. Record data-loss bounds and notify affected operators according
to the incident runbook.
