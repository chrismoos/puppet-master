import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { extractEnrollEnv, readDevEnrollEnv } from "./devAutoEnroll";

// Mock expo-constants with a mutable extra object.
const mockExtra: Record<string, unknown> = {};
vi.mock("expo-constants", () => ({
  default: { expoConfig: { extra: mockExtra } },
}));

describe("devAutoEnroll configuration gating", () => {
  const ORIGINAL_ENV = process.env;

  beforeEach(() => {
    process.env = { ...ORIGINAL_ENV };
    for (const key of Object.keys(mockExtra)) delete mockExtra[key];
    vi.resetModules();
  });

  afterEach(() => {
    process.env = ORIGINAL_ENV;
  });

  // ── app.config.js: build-time injection ──

  it("normal build: no extras injected even with env vars set", () => {
    process.env.PM_DEV_CONTROLLER_URL = "http://localhost:9999";
    process.env.PM_DEV_ENROLL_TOKEN = "tok123";
    delete process.env.PM_UI_TEST_BUILD;

    const appConfig = require("../app.config.js");
    const result = appConfig({ config: {} });

    expect(result.extra?.PM_UI_TEST_BUILD).toBeUndefined();
    expect(result.extra?.PM_DEV_CONTROLLER_URL).toBeUndefined();
    expect(result.extra?.PM_DEV_ENROLL_TOKEN).toBeUndefined();
    expect(result.ios?.bundleIdentifier).toBe("com.tech9.puppetmaster");
  });

  it("UI-test build: injects flag, URL, and token", () => {
    process.env.PM_UI_TEST_BUILD = "1";
    process.env.PM_DEV_CONTROLLER_URL = "http://localhost:9999";
    process.env.PM_DEV_ENROLL_TOKEN = "tok123";

    const appConfig = require("../app.config.js");
    const result = appConfig({ config: {} });

    expect(result.extra?.PM_UI_TEST_BUILD).toBe(true);
    expect(result.extra?.PM_DEV_CONTROLLER_URL).toBe("http://localhost:9999");
    expect(result.extra?.PM_DEV_ENROLL_TOKEN).toBe("tok123");
  });

  it("UI-test build: uses distinct bundle identifier", () => {
    process.env.PM_UI_TEST_BUILD = "1";
    process.env.PM_DEV_CONTROLLER_URL = "http://localhost:9999";
    process.env.PM_DEV_ENROLL_TOKEN = "tok123";

    const appConfig = require("../app.config.js");
    const result = appConfig({ config: {} });

    expect(result.ios?.bundleIdentifier).toBe("com.tech9.puppetmaster.uitest");
  });

  it("UI-test build: fails when controller URL is missing", () => {
    process.env.PM_UI_TEST_BUILD = "1";
    process.env.PM_DEV_ENROLL_TOKEN = "tok123";
    delete process.env.PM_DEV_CONTROLLER_URL;
    const appConfig = require("../app.config.js");
    expect(() => appConfig({ config: {} })).toThrow("PM_DEV_CONTROLLER_URL");
  });

  it("UI-test build: fails when enroll token is missing", () => {
    process.env.PM_UI_TEST_BUILD = "1";
    process.env.PM_DEV_CONTROLLER_URL = "http://localhost:9999";
    delete process.env.PM_DEV_ENROLL_TOKEN;
    const appConfig = require("../app.config.js");
    expect(() => appConfig({ config: {} })).toThrow("PM_DEV_ENROLL_TOKEN");
  });

  it("rejects PM_UI_TEST_BUILD values other than '1'", () => {
    process.env.PM_UI_TEST_BUILD = "true";
    process.env.PM_DEV_CONTROLLER_URL = "http://localhost:9999";
    process.env.PM_DEV_ENROLL_TOKEN = "tok123";
    const appConfig = require("../app.config.js");
    const result = appConfig({ config: {} });
    expect(result.extra?.PM_UI_TEST_BUILD).toBeUndefined();
  });

  // ── extractEnrollEnv: runtime extraction ──

  it("returns env when flag, URL, and token are present", () => {
    const env = extractEnrollEnv({
      PM_UI_TEST_BUILD: true,
      PM_DEV_CONTROLLER_URL: "http://localhost:9999",
      PM_DEV_ENROLL_TOKEN: "tok123",
    });
    expect(env).not.toBeNull();
    expect(env!.controllerUrl).toBe("http://localhost:9999");
    expect(env!.enrollToken).toBe("tok123");
  });

  it("returns null when flag is absent", () => {
    expect(extractEnrollEnv({
      PM_DEV_CONTROLLER_URL: "http://localhost:9999",
      PM_DEV_ENROLL_TOKEN: "tok123",
    })).toBeNull();
  });

  it("returns null when URL is absent", () => {
    expect(extractEnrollEnv({
      PM_UI_TEST_BUILD: true,
      PM_DEV_ENROLL_TOKEN: "tok",
    })).toBeNull();
  });

  it("returns null when token is absent", () => {
    expect(extractEnrollEnv({
      PM_UI_TEST_BUILD: true,
      PM_DEV_CONTROLLER_URL: "http://x",
    })).toBeNull();
  });

  // ── readDevEnrollEnv: fail-closed ──

  it("returns null in normal builds", () => {
    expect(readDevEnrollEnv()).toBeNull();
  });
});
