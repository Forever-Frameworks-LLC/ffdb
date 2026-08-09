# Architecture Decision Log

| ID | Decision | Reason |
| --- | --- | --- |
| ADR-0001 | Rust workspace + pnpm workspace; React/Vite portal | Reproducible, portable builds and a runtime-neutral SDK. |
| ADR-0002 | Axum HTTP API, SQLx PostgreSQL control plane, Rusqlite worker runtime | Mature async HTTP/control-plane stack and direct SQLite authorizer/progress access. |
| ADR-0003 | One SQLite file per opaque project database id | Required isolation and simple backup/restore routing. No first-class environments. |
| ADR-0004 | Worker subprocess protocol over length-bounded local IPC | A pathological SQLite workload cannot crash the API/control-plane process. |
| ADR-0005 | Ed25519 JWT signing with `kid`, platform-managed encrypted private keys, overlapping public-key rotation | Asymmetric verification and safe staged rotation; project-specific providers remain pluggable. |
| ADR-0006 | Argon2id PHC password hashes with versioned parameters | Memory-hard hashing with transparent future rehash. |
| ADR-0007 | Random 256-bit API/refresh tokens; store only keyed hashes | Database disclosure does not reveal bearer credentials; lookup prefixes are non-secret. |
| ADR-0008 | RLS physical tables + public views + generated triggers + authorizer | Defense in depth for reads/writes and strict internal namespace protection. |
| ADR-0009 | Server-sequenced logical changes, not WAL replication | RLS filtering, schema evolution, heterogeneous clients, and stable cursors. |
| ADR-0010 | LWW uses server sequence; client time is metadata only | Deterministic convergence without trusting client clocks. |
| ADR-0011 | Email source compiles in an isolated CLI/build job; request handling renders precompiled templates only | Prevent arbitrary JavaScript execution in the API process. |
| ADR-0012 | S3 presigning is a provider adapter and always follows SQLite RLS evaluation | Avoid a storage confused deputy and duplicated authorization logic. |
| ADR-0013 | Unix epoch milliseconds on the wire; SQLite integer safety rules are explicit | Cross-runtime deterministic JSON encoding. |
| ADR-0014 | Stable API errors use namespaced string codes plus request id and safe details | SDK compatibility and observability without leaking internals. |
| ADR-0015 | Rust 1.96 / edition 2024 and Node 24 / pnpm 11 are the development baseline | Matches the available toolchain; CI also checks stable and ARM64 where available. |
| ADR-0016 | Native ingress is one Caddy process proxying directly to one Axum worker supervisor per node | Avoid a redundant Caddy-to-nginx stage and prevent unsafe duplicate ownership of project SQLite workers. |
| ADR-0017 | Forwarded client addresses are accepted only from explicit trusted proxy CIDRs | Preserve per-client rate limits and audit identity without trusting attacker-supplied forwarding headers. |

## Defaults that unblock implementation

- Local ports: API `8080`, portal `5173`, PostgreSQL `5432`, MinIO `9000/9001`.
- Default limits: SQL 256 KiB, 999 variables, 10,000 rows, 8 MiB response,
  5 s statement, 15 s transaction, one active request per project worker, 32
  admitted requests per configured worker,
  1 GiB SQLite file, 5 GiB object bytes, 64 MiB object, 1000 sync changes/pull.
- Access tokens live 15 minutes; refresh tokens 30 days with rotation and family
  reuse detection; verification/reset tokens 30 minutes; signed object URLs 5 minutes.
- Cursor/tombstone retention is 30/90 days. Expired cursors require a snapshot.
- PostgreSQL advisory locks protect project lifecycle transitions; a worker lease
  plus fencing generation prevents stale workers from writing after reroute.
- Configurable provider URLs allow HTTPS only outside explicitly enabled local
  development mode and are checked against an SSRF allowlist.
