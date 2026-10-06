import { describe, expect, it } from "vitest";
import { paginate } from "./pagination";

describe("paginate", () => {
  const rows = Array.from({ length: 126 }, (_, index) => index);

  it("names the range it shows and how many pages there are", () => {
    expect(paginate(rows, 0, 25)).toMatchObject({ from: 1, to: 25, total: 126, page: 0, pages: 6 });
    expect(paginate(rows, 5, 25)).toMatchObject({ from: 126, to: 126, rows: [125] });
  });

  it("pulls a page past the end back to the last one", () => {
    expect(paginate(rows, 40, 50)).toMatchObject({ page: 2, from: 101, to: 126 });
  });

  it("shows an empty range for no rows", () => {
    expect(paginate([], 3, 25)).toMatchObject({ from: 0, to: 0, total: 0, page: 0, pages: 1 });
  });
});
