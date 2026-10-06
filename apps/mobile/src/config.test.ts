import { describe, expect, it } from "vitest";

import type { KeyValueStorage } from "@puppet-master/client-core/platform";

import { readControllerConfig, writeControllerConfig } from "./config";

function memoryStorage(): KeyValueStorage {
  const map = new Map<string, string>();
  return {
    getItem: (key) => map.get(key) ?? null,
    setItem: (key, value) => void map.set(key, value),
  };
}

describe("controller config", () => {
  it("round-trips through storage", () => {
    const storage = memoryStorage();
    writeControllerConfig(storage, {
      baseUrl: "https://pm.example",
      sessionToken: "tok",
      pushPreviewsEnabled: false,
    });
    expect(readControllerConfig(storage)).toEqual({
      baseUrl: "https://pm.example",
      sessionToken: "tok",
      pushPreviewsEnabled: false,
    });
  });

  it("returns null until a base URL is saved", () => {
    expect(readControllerConfig(memoryStorage())).toBeNull();
  });

  it("reads a cleared token back as null", () => {
    const storage = memoryStorage();
    writeControllerConfig(storage, {
      baseUrl: "https://pm.example",
      sessionToken: "old",
      pushPreviewsEnabled: false,
    });
    writeControllerConfig(storage, {
      baseUrl: "https://pm.example",
      sessionToken: null,
      pushPreviewsEnabled: false,
    });
    expect(readControllerConfig(storage)).toEqual({
      baseUrl: "https://pm.example",
      sessionToken: null,
      pushPreviewsEnabled: false,
    });
  });

  it("persists the push previews preference", () => {
    const storage = memoryStorage();
    writeControllerConfig(storage, {
      baseUrl: "https://pm.example",
      sessionToken: null,
      pushPreviewsEnabled: true,
    });
    const config = readControllerConfig(storage);
    expect(config?.pushPreviewsEnabled).toBe(true);
  });
});
