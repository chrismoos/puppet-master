import { describe, expect, it } from "vitest";

import { shouldBearerReInit } from "./bearerReconnect";

describe("shouldBearerReInit", () => {
  it("returns true when the token changed (refresh was needed)", async () => {
    const result = await shouldBearerReInit(
      () => "old-token",
      () => Promise.resolve("new-token"),
    );
    expect(result).toBe(true);
  });

  it("returns false when the token is unchanged (still valid)", async () => {
    const result = await shouldBearerReInit(
      () => "same-token",
      () => Promise.resolve("same-token"),
    );
    expect(result).toBe(false);
  });

  it("returns true when the current token is null (not yet available)", async () => {
    const result = await shouldBearerReInit(
      () => null,
      () => Promise.resolve("fresh-token"),
    );
    expect(result).toBe(true);
  });
});
