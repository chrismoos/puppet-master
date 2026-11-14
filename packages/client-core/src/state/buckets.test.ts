import { describe, expect, it } from "vitest";
import { bucketMonogram, railMonograms } from "./buckets";

describe("bucket letters", () => {
  it("takes two words' initials, or one word's first two letters", () => {
    expect(bucketMonogram("puppet master")).toBe("PM");
    expect(bucketMonogram("fullstack")).toBe("FU");
  });

  it("keeps letters distinct when two buckets would share them", () => {
    const letters = railMonograms([{ id: "1", name: "test" }, { id: "2", name: "tech9" }]);
    expect(letters.get("1")).toBe("TE");
    expect(letters.get("2")).not.toBe("TE");
  });
});
