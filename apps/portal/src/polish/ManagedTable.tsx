import { isValidElement, useEffect, useMemo, useState, type ReactNode } from "react";

import { Icon } from "../icons.js";
import "./managed-table.css";

export interface ManagedTableProps {
  readonly headings: readonly string[];
  readonly rows: readonly (readonly ReactNode[])[];
  readonly empty?: string;
  readonly label?: string;
  readonly searchable?: boolean;
  readonly pagination?: boolean;
  readonly pageSizes?: readonly number[];
}

export function ManagedTable({
  headings,
  rows,
  empty = "No results.",
  label = "records",
  searchable = true,
  pagination = true,
  pageSizes = [10, 25, 50],
}: ManagedTableProps) {
  const [query, setQuery] = useState("");
  const [sortIndex, setSortIndex] = useState<number | null>(null);
  const [descending, setDescending] = useState(false);
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState(pageSizes[0] ?? 10);

  const normalizedQuery = query.trim().toLocaleLowerCase();
  const filtered = useMemo(() => {
    const matches = normalizedQuery === ""
      ? [...rows]
      : rows.filter((row) => row.some((cell) => cellText(cell).toLocaleLowerCase().includes(normalizedQuery)));
    if (sortIndex === null) return matches;
    return matches.sort((left, right) => {
      const compared = cellText(left[sortIndex]).localeCompare(cellText(right[sortIndex]), undefined, { numeric: true, sensitivity: "base" });
      return descending ? -compared : compared;
    });
  }, [descending, normalizedQuery, rows, sortIndex]);

  const pageCount = pagination ? Math.max(1, Math.ceil(filtered.length / pageSize)) : 1;
  const safePage = Math.min(page, pageCount);
  const visible = pagination ? filtered.slice((safePage - 1) * pageSize, safePage * pageSize) : filtered;
  useEffect(() => { setPage(1); }, [normalizedQuery, pageSize, rows.length]);

  const toggleSort = (index: number) => {
    if (sortIndex === index) setDescending((value) => !value);
    else { setSortIndex(index); setDescending(false); }
    setPage(1);
  };

  return (
    <div className="managed-table-workspace">
      {searchable && rows.length > 0 ? (
        <div className="managed-table-toolbar">
          <label className="managed-table-search">
            <Icon name="search" size={15} />
            <span className="sr-only">Search {label}</span>
            <input value={query} onChange={(event) => setQuery(event.target.value)} placeholder={`Search ${label}`} type="search" />
          </label>
          <span>{filtered.length} {filtered.length === 1 ? label.replace(/s$/u, "") : label}</span>
          {query === "" ? null : <button type="button" onClick={() => setQuery("")}>Clear</button>}
        </div>
      ) : null}
      {rows.length === 0 ? <div className="managed-table-empty">{empty}</div> : filtered.length === 0 ? <div className="managed-table-empty">No {label} match the current search.</div> : (
        <div className="management-table managed-table-scroll portal-table-scroll" role="region" aria-label={label} tabIndex={0}>
          <table aria-label={label}>
            <thead><tr>{headings.map((heading, index) => (
              <th aria-sort={sortIndex === index ? (descending ? "descending" : "ascending") : "none"} key={heading}>
                <button type="button" onClick={() => toggleSort(index)}>{heading}<Icon name={sortIndex === index ? (descending ? "chevronDown" : "chevronUp") : "chevronDown"} size={13} /></button>
              </th>
            ))}</tr></thead>
            <tbody>{visible.map((row, rowIndex) => <tr key={`${safePage}-${rowIndex}`}>{row.map((cell, cellIndex) => <td data-label={headings[cellIndex]} key={cellIndex}>{cell}</td>)}</tr>)}</tbody>
          </table>
        </div>
      )}
      {filtered.length === 0 || !pagination ? null : (
        <div className="managed-table-footer">
          <span>Showing {(safePage - 1) * pageSize + 1}–{Math.min(safePage * pageSize, filtered.length)} of {filtered.length}</span>
          <div>
            <label>Rows <select aria-label={`Rows per page for ${label}`} value={pageSize} onChange={(event) => setPageSize(Number(event.target.value))}>{pageSizes.map((size) => <option key={size} value={size}>{size}</option>)}</select></label>
            <button aria-label={`Previous page of ${label}`} disabled={safePage === 1} type="button" onClick={() => setPage((value) => Math.max(1, value - 1))}><Icon name="chevronRight" className="flip" size={15} /></button>
            <span>Page {safePage} of {pageCount}</span>
            <button aria-label={`Next page of ${label}`} disabled={safePage === pageCount} type="button" onClick={() => setPage((value) => Math.min(pageCount, value + 1))}><Icon name="chevronRight" size={15} /></button>
          </div>
        </div>
      )}
    </div>
  );
}

function cellText(value: ReactNode): string {
  if (value === null || value === undefined || typeof value === "boolean") return "";
  if (typeof value === "string" || typeof value === "number" || typeof value === "bigint") return String(value);
  if (Array.isArray(value)) return value.map(cellText).join(" ");
  if (isValidElement<{ readonly children?: ReactNode; readonly value?: unknown; readonly title?: unknown }>(value)) {
    return [value.props.title, value.props.value, value.props.children].map((item) => cellText(item as ReactNode)).join(" ");
  }
  return "";
}
