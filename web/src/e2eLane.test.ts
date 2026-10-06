import { describe, expect, it } from "vitest";
import { validateLaneBinary } from "../e2e/laneContract";

describe("browser lane binary contract", () => {
  it.each(["worker", "integration"] as const)("accepts e2e profile for %s", (lane) => {
    expect(() => validateLaneBinary(lane, "/tmp/target/e2e/pm")).not.toThrow();
    expect(() => validateLaneBinary(lane, "/tmp/target/release/pm")).toThrow(/e2e Cargo profile/);
  });

  it("reserves the release profile for performance", () => {
    expect(() => validateLaneBinary("performance", "/tmp/target/release/pm")).not.toThrow();
    expect(() => validateLaneBinary("performance", "/tmp/target/e2e/pm")).toThrow(/release Cargo profile/);
  });
});
