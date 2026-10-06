import { describe, expect, it } from "vitest";
import { PermissionMode } from "../gen/pm/v1/pm_pb";
import { permissionTag, resolvePermissionMode } from "./permission";

const { UNSPECIFIED, DEFAULT, AUTO, BYPASS } = PermissionMode;

describe("resolvePermissionMode", () => {
  it("uses the spawn override when set", () => {
    expect(resolvePermissionMode(BYPASS, AUTO, DEFAULT)).toBe(BYPASS);
  });

  it("falls back to the project override when the spawn inherits", () => {
    expect(resolvePermissionMode(UNSPECIFIED, AUTO, DEFAULT)).toBe(AUTO);
  });

  it("falls back to the bucket default when spawn and project inherit", () => {
    expect(resolvePermissionMode(UNSPECIFIED, UNSPECIFIED, BYPASS)).toBe(BYPASS);
  });

  it("treats an all-inherit chain as default", () => {
    expect(resolvePermissionMode(UNSPECIFIED, UNSPECIFIED, UNSPECIFIED)).toBe(DEFAULT);
  });

  it("prefers a project override over the bucket default", () => {
    expect(resolvePermissionMode(UNSPECIFIED, DEFAULT, BYPASS)).toBe(DEFAULT);
  });
});

describe("permissionTag", () => {
  it("has no tag for default or inherit", () => {
    expect(permissionTag(DEFAULT)).toBeNull();
    expect(permissionTag(UNSPECIFIED)).toBeNull();
  });

  it("tags auto and bypass", () => {
    expect(permissionTag(AUTO)?.label).toBe("auto");
    expect(permissionTag(BYPASS)?.label).toBe("bypass");
  });
});
