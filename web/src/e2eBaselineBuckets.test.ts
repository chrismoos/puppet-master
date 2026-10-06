import { describe, expect, it, vi } from "vitest";
import { establishBrowserBucket } from "../e2e/baselineBuckets";

describe("browser E2E baseline buckets", () => {
  it("deliberately replaces the production seed with one deterministic fixture bucket", () => {
    const cli = vi.fn()
      .mockReturnValueOnce("ID     NAME\n1      Default\n")
      .mockReturnValueOnce("project 1 deleted\n")
      .mockReturnValueOnce("bucket 1 deleted\n")
      .mockReturnValueOnce("bucket 1 created\n")
      .mockReturnValueOnce("ID     NAME\n1      browser-e2e\n");

    expect(establishBrowserBucket(cli)).toBe(1);
    expect(cli.mock.calls).toEqual([
      [["bucket", "ls"]],
      [["project", "rm", "1"]],
      [["bucket", "rm", "1"]],
      [["bucket", "add", "browser-e2e"]],
      [["bucket", "ls"]],
    ]);
  });

  it("refuses to mutate a baseline that is not a fresh production seed", () => {
    const cli = vi.fn().mockReturnValue("ID     NAME\n1      user-bucket\n");

    expect(() => establishBrowserBucket(cli)).toThrow(/expected fresh Default bucket/);
    expect(cli).toHaveBeenCalledTimes(1);
  });
});
