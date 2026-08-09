# Row-Level Security Semantics

FFDB accepts a documented subset of PostgreSQL policy DDL:

```sql
ALTER TABLE documents ENABLE ROW LEVEL SECURITY;
ALTER TABLE documents FORCE ROW LEVEL SECURITY;

CREATE POLICY documents_read ON documents
  AS PERMISSIVE FOR SELECT TO authenticated
  USING (organization_id = auth.claim('organization_id'));

CREATE POLICY documents_write ON documents
  FOR INSERT TO authenticated
  WITH CHECK (owner_id = auth.uid());
```

`ALTER TABLE ... DISABLE ROW LEVEL SECURITY`, `NO FORCE ROW LEVEL SECURITY`,
`ALTER POLICY`, and `DROP POLICY [IF EXISTS]` are also supported by the custom
parser. Identifiers are normalized and always rendered with SQLite-safe quoting.
Policy predicates are parsed internally; no client-supplied AST is accepted.

## Combination

For the current command and role, permissive policies combine with OR and
restrictive policies with AND. The result is `(any permissive) AND (all
restrictive)`. If RLS is enabled and no applicable permissive policy exists, the
result is false. `FOR ALL` participates in every command. `public` targets every
role; other targets match `auth.role()`.

- SELECT uses `USING` to filter visible rows.
- DELETE uses `USING` to select deletable rows.
- INSERT uses `WITH CHECK` to validate each new row.
- UPDATE requires both `USING` for the old row and `WITH CHECK` for the new row.
- If an UPDATE/INSERT policy omits `WITH CHECK`, its `USING` predicate is used as
  the check where PostgreSQL semantics call for that default.

Disabling RLS removes policy filtering. When enabled but not forced, verified
developer administrative DML may bypass policy predicates. `FORCE` removes that
bypass. End-user mode never gains a bypass.

## Implementation boundary

Logical tables become public views over protected `__ffdb_` backing tables.
Generated `INSTEAD OF` triggers enforce writes. Immutable connection-local auth
functions provide verified context. The SQLite authorizer denies direct internal
access and trusts only exact compiler-generated object names held in connection
state; caller-created lookalike views/triggers are denied.

## Intentional PostgreSQL differences

- SQLite type affinity, collation, NULL behavior, generated columns, conflict
  handling, trigger order, and query planning remain SQLite behavior.
- Policy expressions support the documented safe SQLite expression subset plus
  FFDB auth functions, not arbitrary PostgreSQL functions/operators/casts.
- PostgreSQL table owners/superusers and `BYPASSRLS` roles do not exist. Only the
  explicit developer/force behavior above applies.
- Policy DDL is compiled during an FFDB migration; SQLite never parses it.
- Constraint errors are normalized to a generic failure: protected table names,
  column names, values, and raw SQLite details are not returned. The failure bit
  itself can still act as an existence oracle when a caller probes a primary-key,
  unique, or foreign-key value belonging to an RLS-hidden row. FFDB therefore does
  not promise strong noninterference across schema constraints. Prefer opaque,
  high-entropy identifiers and tenant-scoped composite keys, and rate-limit
  attacker-controlled writes where this distinction matters.
- Policy/schema changes can require offline clients to resnapshot.

The compatibility tests are the normative behavior. Applications must not depend
on undocumented PostgreSQL behavior.

## Current write-shape restrictions

FFDB fails closed where SQLite views cannot reproduce PostgreSQL table behavior:

- SQLite does not support `UPSERT` (`ON CONFLICT ... DO UPDATE`) against a view,
  so UPSERT on an RLS-protected logical table is rejected by SQLite. Use a bounded
  transaction containing an UPDATE followed by a conditional INSERT.
- An `INSTEAD OF` trigger cannot distinguish an omitted view column from an
  explicitly supplied NULL. Callers must currently provide values for columns
  whose backing-table defaults they intend to use. NOT NULL constraints still
  fail closed; FFDB does not silently substitute a default for explicit NULL.
- Generated columns are exposed for reads but excluded from generated INSERT and
  UPDATE assignments. SQLite computes them on the backing table.
- Every row of `INSERT ... SELECT`, bulk UPDATE, and bulk DELETE passes through the
  generated trigger checks. A failing row aborts and rolls back the statement.
- `RETURNING` is supported to the extent SQLite supports it for the generated
  view/trigger statement. Returned rows never make direct backing-table access
  permissible.
- A protected logical table cannot currently be converted back into an ordinary
  physical table by `DISABLE ROW LEVEL SECURITY`; disabling removes filtering but
  retains the protected view/backing layout. Migration down SQL must account for
  this and must not attempt to name the backing object.

These are compatibility differences, not authorization exceptions. Unsupported
forms return an error and never fall back to unprotected execution.
