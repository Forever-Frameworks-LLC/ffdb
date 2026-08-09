import { useEffect, useMemo, useState, type ReactNode } from "react";
import { ArrowDown, ArrowUp, ArrowUpDown, FilterX, Search } from "lucide-react";

import "./database-activity.css";

export interface PolishedDataColumn<Row> {
  /** Stable column identifier used by sorting. */
  readonly key: string;
  readonly label: string;
  /** Primitive value used for sorting and the default cell renderer. */
  readonly value: (row: Row) => string | number | null;
  /** Optional richer visual renderer. */
  readonly render?: (row: Row) => ReactNode;
  readonly sortable?: boolean;
  readonly className?: string;
}

export interface PolishedDataFilter<Row> {
  readonly key: string;
  readonly label: string;
  readonly options: readonly { readonly value: string; readonly label: string }[];
  /** Return the option value that represents a row. */
  readonly value: (row: Row) => string;
}

export interface PolishedDataTableProps<Row> {
  readonly rows: readonly Row[];
  readonly columns: readonly PolishedDataColumn<Row>[];
  readonly rowKey: (row: Row) => string;
  /** Full-text search input. Defaults to all primitive column values. */
  readonly searchText?: (row: Row) => string;
  readonly searchPlaceholder?: string;
  readonly filters?: readonly PolishedDataFilter<Row>[];
  readonly defaultSort?: { readonly key: string; readonly direction: "asc" | "desc" };
  readonly pageSizes?: readonly number[];
  readonly emptyTitle?: string;
  readonly emptyDetail?: string;
  /** Final action cell. Buttons should produce a real navigation, dialog, or mutation. */
  readonly actions?: (row: Row) => ReactNode;
}

/**
 * Shared zero-dependency table for portal management pages. It owns text search,
 * select filters, stable sorting, paging, reset, accessible headers, and the
 * responsive data-label markup used by database-activity.css.
 */
export function PolishedDataTable<Row>({
  rows,
  columns,
  rowKey,
  searchText,
  searchPlaceholder = "Search…",
  filters = [],
  defaultSort,
  pageSizes = [10, 25, 50],
  emptyTitle = "No results",
  emptyDetail = "Try adjusting the current search or filters.",
  actions,
}: PolishedDataTableProps<Row>) {
  const [search, setSearch] = useState("");
  const [filterValues, setFilterValues] = useState<Readonly<Record<string, string>>>({});
  const [sortKey, setSortKey] = useState(defaultSort?.key ?? columns.find((column) => column.sortable !== false)?.key ?? "");
  const [direction, setDirection] = useState<"asc" | "desc">(defaultSort?.direction ?? "asc");
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState(pageSizes[0] ?? 10);

  useEffect(() => { setPage(1); }, [direction, filterValues, pageSize, search, sortKey]);

  const filtered = useMemo(() => {
    const needle = search.trim().toLocaleLowerCase();
    const result = rows.filter((row) => {
      if (needle !== "") {
        const haystack = searchText?.(row) ?? columns.map((column) => String(column.value(row) ?? "")).join(" ");
        if (!haystack.toLocaleLowerCase().includes(needle)) return false;
      }
      return filters.every((filter) => {
        const current = filterValues[filter.key] ?? "all";
        return current === "all" || filter.value(row) === current;
      });
    });
    const column = columns.find((candidate) => candidate.key === sortKey);
    if (column === undefined) return result;
    return [...result].sort((left, right) => {
      const a = column.value(left); const b = column.value(right);
      const compared = typeof a === "number" && typeof b === "number" ? a - b : String(a ?? "").localeCompare(String(b ?? ""));
      return direction === "asc" ? compared : -compared;
    });
  }, [columns, direction, filterValues, filters, rows, search, searchText, sortKey]);

  const pageCount = Math.max(1, Math.ceil(filtered.length / pageSize));
  const safePage = Math.min(page, pageCount);
  const visible = filtered.slice((safePage - 1) * pageSize, safePage * pageSize);
  const hasFilters = search !== "" || Object.values(filterValues).some((value) => value !== "all");
  const reset = () => { setSearch(""); setFilterValues({}); };
  const changeSort = (key: string) => { if (sortKey === key) setDirection((current) => current === "asc" ? "desc" : "asc"); else { setSortKey(key); setDirection("asc"); } };

  return (
    <div className="ffdb-polished-table">
      <div className="ffdb-filter-bar" role="search" aria-label="Filter table">
        <label className="ffdb-search-field"><Search size={15} /><span className="ffdb-sr-only">Search table</span><input value={search} onChange={(event) => setSearch(event.target.value)} placeholder={searchPlaceholder} /></label>
        {filters.map((filter) => <label key={filter.key}><span className="ffdb-sr-only">{filter.label}</span><select aria-label={filter.label} value={filterValues[filter.key] ?? "all"} onChange={(event) => setFilterValues((current) => ({ ...current, [filter.key]: event.target.value }))}><option value="all">All {filter.label.toLocaleLowerCase()}</option>{filter.options.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}</select></label>)}
        <button className="ffdb-button ffdb-button-quiet" type="button" disabled={!hasFilters} onClick={reset}><FilterX size={15} /> Reset</button>
      </div>
      {visible.length === 0 ? <div className="ffdb-empty-state"><span><Search size={20} /></span><h3>{emptyTitle}</h3><p>{rows.length === 0 ? emptyDetail : "No rows match the current search and filters."}</p>{hasFilters ? <button className="ffdb-button ffdb-button-secondary" type="button" onClick={reset}>Clear filters</button> : null}</div> : <>
        <div className="ffdb-table-wrap portal-table-scroll" role="region" aria-label="Filtered records" tabIndex={0}><table className="ffdb-data-table"><thead><tr>{columns.map((column) => <th className={column.className} key={column.key} aria-sort={sortKey === column.key ? direction === "asc" ? "ascending" : "descending" : "none"}>{column.sortable === false ? column.label : <button type="button" onClick={() => changeSort(column.key)}>{column.label}{sortKey !== column.key ? <ArrowUpDown size={13} /> : direction === "asc" ? <ArrowUp size={13} /> : <ArrowDown size={13} />}</button>}</th>)}{actions === undefined ? null : <th><span className="ffdb-sr-only">Actions</span></th>}</tr></thead><tbody>{visible.map((row) => <tr key={rowKey(row)}>{columns.map((column) => <td className={column.className} key={column.key} data-label={column.label}>{column.render?.(row) ?? String(column.value(row) ?? "—")}</td>)}{actions === undefined ? null : <td className="ffdb-row-action" data-label="Actions">{actions(row)}</td>}</tr>)}</tbody></table></div>
        <footer className="ffdb-pagination"><span>{filtered.length === 0 ? 0 : (safePage - 1) * pageSize + 1}–{Math.min(safePage * pageSize, filtered.length)} of {filtered.length.toLocaleString()}</span><div><label><span>Rows</span><select aria-label="Rows per page" value={pageSize} onChange={(event) => setPageSize(Number(event.target.value))}>{pageSizes.map((size) => <option key={size} value={size}>{size}</option>)}</select></label><button type="button" aria-label="Previous page" disabled={safePage <= 1} onClick={() => setPage(safePage - 1)}>‹</button><span>Page {safePage} of {pageCount}</span><button type="button" aria-label="Next page" disabled={safePage >= pageCount} onClick={() => setPage(safePage + 1)}>›</button></div></footer>
      </>}
    </div>
  );
}
