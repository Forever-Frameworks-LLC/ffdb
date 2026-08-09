# SQL Support and Hosted Restrictions

End-user mode accepts exactly one prepared `SELECT`, `INSERT`, `UPDATE`, or
`DELETE` statement. A leading ordinary or recursive CTE is classified by its outer
statement. Comments, strings, and trigger bodies cannot smuggle a second
statement. The transaction API accepts a bounded list and owns `BEGIN`, savepoint,
commit, and rollback.

Developer migrations support ordinary SQLite table/index/view/trigger DDL and the
custom RLS statements, subject to authorizer and namespace rules. Explicit up and
down SQL are required.

## Always prohibited in hosted SQL

- arbitrary `ATTACH`/`DETACH` or database filenames;
- `load_extension` and dynamic extension loading;
- `PRAGMA writable_schema`, direct `sqlite_schema` mutation, and writable internal
  metadata;
- `VACUUM INTO` or any statement accepting a host path;
- caller objects in the reserved `__ffdb_` namespace;
- unsupported virtual table modules or module arguments that access the host;
- caller-owned transaction control in end-user mode;
- direct reads/writes of physical backing, policy, migration, sync, or storage
  metadata objects.

`PRAGMA foreign_keys`, WAL/synchronous/checkpoint policy, trusted schema, defensive
mode, variable/SQL/expression/trigger depth, and extension capabilities are owned
by the worker. JSON and FTS5 may be statically enabled. UUID/vector/crypto helpers
must be versioned, deterministic where required, audited, and explicitly allowed;
arbitrary extensions never become available through SQL.

Requests are bounded by SQL bytes, variables, statement/transaction deadline,
progress cancellation, rows, response bytes, database bytes, concurrency, and
queue size. A parser allowance is not authority: SQLite preparation and the
authorizer must also allow every engine action.

## Statement matrix

| Form | End user | Developer/migration | Notes |
| --- | --- | --- | --- |
| `SELECT`, including recursive CTE | Yes | Yes | One prepared statement; deadline and row/byte bounds apply. |
| `INSERT`, `UPDATE`, `DELETE` | Yes | Yes | RLS view and generated triggers remain authoritative. |
| `RETURNING` | Yes | Yes | Subject to SQLite view/trigger behavior and response limits. |
| `CREATE/ALTER/DROP` table/index/view/trigger | No | Constrained | Reserved or protected generated objects cannot be targeted. |
| Custom policy/RLS DDL | No | Migration only | Parsed and compiled before SQLite execution. |
| `PRAGMA` | No | Read-only/small allowlist | Worker-owned safety pragmas cannot be changed. |
| `ATTACH`, `DETACH`, `VACUUM INTO` | No | No | Host-path escape boundary. |
| Caller `BEGIN`/`COMMIT`/savepoints | No | No in query SQL | The transaction endpoint and migration engine own transaction control. |
| Virtual tables | No | FTS5 allowlist only | No dynamic modules or arbitrary extension loading. |

Custom RLS syntax is accepted only as migration statements. A query request that
contains policy DDL receives `query.statement_not_allowed`; it is never passed to
SQLite as if SQLite understood PostgreSQL policy grammar.
