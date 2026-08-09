# JSON SQL Encoding

The JSON SQL format is deterministic across browser, Node.js, React Native, and
Rust runtimes. Clients never send a parsed SQL tree; the server parses and
prepares the supplied string.

## Parameters

| Type | JSON representation | Validation |
| --- | --- | --- |
| null | `{"type":"null"}` | no value member |
| integer | `{"type":"integer","value":42}` or decimal string | signed 64-bit only |
| real | `{"type":"real","value":3.5}` | finite IEEE-754 only |
| text | `{"type":"text","value":"..."}` | valid JSON/UTF-8 and request limit |
| blob | `{"type":"blob","value":"AQI="}` | canonical base64 and request limit |

Parameters are bound through SQLite APIs. They are never interpolated into SQL.

## Results

The `columns` array defines the positional cells in every row. This avoids object
key collision when a query returns duplicate aliases. Runtime values are encoded:

- `NULL` as JSON null;
- integers inside ±9,007,199,254,740,991 as numbers;
- larger signed 64-bit integers as decimal strings;
- finite REAL values as numbers;
- TEXT as strings;
- BLOB as `{"$blob":"<base64>"}`.

NaN and infinities are rejected rather than silently converted. Each row must
contain exactly the number of cells advertised by `columns`. A response that
reaches its byte or row bound either reports `truncated: true` for explicitly
truncatable reads or fails closed; mutations are never partially represented as a
successful response.

## Cancellation and transaction behavior

Disconnect and `AbortSignal` cancellation propagate to the worker progress
handler. A one-statement query is server wrapped as needed. The transaction API
executes its bounded list on one connection and returns all statement results only
after commit. Caller SQL cannot contain transaction-control statements.
