export type E2eLane = "worker" | "integration" | "performance";

export function validateLaneBinary(lane: E2eLane, binary: string): void {
  const segments = binary.replaceAll("\\", "/").split("/");
  const profile = segments.at(-2);
  const expected = lane === "performance" ? "release" : "e2e";
  if (profile !== expected) {
    throw new Error(`PM_E2E_PM_BIN for the ${lane} lane must use the ${expected} Cargo profile`);
  }
}
