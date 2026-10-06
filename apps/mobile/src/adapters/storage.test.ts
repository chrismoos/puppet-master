import { describe, expect, it } from "vitest";

import { CachedSecureStorage, type AsyncStringStore } from "./storage";

function memoryBackend(initial: Record<string, string> = {}) {
  const stored = new Map(Object.entries(initial));
  const writes: Array<{ key: string; value: string }> = [];
  const backend: AsyncStringStore = {
    getItemAsync: (key) => Promise.resolve(stored.get(key) ?? null),
    setItemAsync: (key, value) => {
      writes.push({ key, value });
      stored.set(key, value);
      return Promise.resolve();
    },
  };
  return { backend, stored, writes };
}

describe("CachedSecureStorage", () => {
  it("hydrates listed keys and serves them synchronously", async () => {
    const { backend } = memoryBackend({ "controller.baseUrl": "https://pm.example", other: "x" });
    const storage = await CachedSecureStorage.hydrate(backend, ["controller.baseUrl", "missing"]);
    expect(storage.getItem("controller.baseUrl")).toBe("https://pm.example");
    expect(storage.getItem("missing")).toBeNull();
    expect(storage.getItem("other")).toBeNull();
  });

  it("writes through to the backend in issue order", async () => {
    const { backend, writes } = memoryBackend();
    const storage = await CachedSecureStorage.hydrate(backend, []);
    storage.setItem("a", "1");
    storage.setItem("a", "2");
    storage.setItem("b", "3");
    expect(storage.getItem("a")).toBe("2");
    await storage.flush();
    expect(writes).toEqual([
      { key: "a", value: "1" },
      { key: "a", value: "2" },
      { key: "b", value: "3" },
    ]);
  });

  it("keeps the cached value and keeps flushing after a backend failure", async () => {
    const errors: string[] = [];
    const writes: string[] = [];
    const backend: AsyncStringStore = {
      getItemAsync: () => Promise.resolve(null),
      setItemAsync: (key) => {
        if (key === "bad") return Promise.reject(new Error("keychain unavailable"));
        writes.push(key);
        return Promise.resolve();
      },
    };
    const storage = await CachedSecureStorage.hydrate(backend, [], (key) => errors.push(key));
    storage.setItem("bad", "v");
    storage.setItem("good", "v");
    await storage.flush();
    expect(storage.getItem("bad")).toBe("v");
    expect(errors).toEqual(["bad"]);
    expect(writes).toEqual(["good"]);
  });

  it("reports hydration failures per key without aborting", async () => {
    const errors: string[] = [];
    const backend: AsyncStringStore = {
      getItemAsync: (key) =>
        key === "broken" ? Promise.reject(new Error("boom")) : Promise.resolve("ok"),
      setItemAsync: () => Promise.resolve(),
    };
    const storage = await CachedSecureStorage.hydrate(backend, ["broken", "fine"], (key) =>
      errors.push(key),
    );
    expect(errors).toEqual(["broken"]);
    expect(storage.getItem("fine")).toBe("ok");
  });
});
