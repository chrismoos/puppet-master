export const PAGE_SIZES = [25, 50, 100] as const;
export const DEFAULT_PAGE_SIZE = PAGE_SIZES[0];

export interface Page<T> {
  rows: T[];
  page: number;
  pages: number;
  /** One-based position of the first and last row shown; both zero when empty. */
  from: number;
  to: number;
  total: number;
}

/** One page of `rows`, with a page past the end pulled back to the last one. */
export function paginate<T>(rows: readonly T[], page: number, size: number): Page<T> {
  const pages = Math.max(1, Math.ceil(rows.length / size));
  const current = Math.min(Math.max(0, page), pages - 1);
  const start = current * size;
  const shown = rows.slice(start, start + size);
  return {
    rows: shown,
    page: current,
    pages,
    from: shown.length ? start + 1 : 0,
    to: start + shown.length,
    total: rows.length,
  };
}
