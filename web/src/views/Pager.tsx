import { PAGE_SIZES, type Page } from "./pagination";

export function Pager({
  page,
  onPage,
  size,
  onSize,
  noun,
  compact,
}: {
  page: Page<unknown>;
  onPage: (page: number) => void;
  size?: number;
  onSize?: (size: number) => void;
  /** What the rows are, when the count needs saying: "3 unclassified". */
  noun?: string;
  /** The narrow panel drops the page count and shortens the buttons. */
  compact?: boolean;
}) {
  return (
    <div className="ui-pager">
      <span>
        {page.from}–{page.to} of {page.total}
        {noun ? ` ${noun}` : ""}
      </span>
      <span className="grow" />
      {size !== undefined && onSize && (
        <>
          <span>Rows</span>
          <select
            aria-label="Rows per page"
            value={size}
            onChange={(event) => onSize(Number(event.target.value))}
          >
            {PAGE_SIZES.map((option) => (
              <option key={option}>{option}</option>
            ))}
          </select>
        </>
      )}
      <button
        type="button"
        className="btn"
        disabled={page.page === 0}
        onClick={() => onPage(page.page - 1)}
      >
        {compact ? "Prev" : "Previous"}
      </button>
      {!compact && (
        <span>
          Page {page.page + 1} of {page.pages}
        </span>
      )}
      <button
        type="button"
        className="btn"
        disabled={page.page + 1 >= page.pages}
        onClick={() => onPage(page.page + 1)}
      >
        Next
      </button>
    </div>
  );
}
