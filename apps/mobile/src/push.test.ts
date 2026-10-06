import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// push.ts reaches straight into native modules, so each is stubbed to
// the smallest surface the flow touches.
const notifications = vi.hoisted(() => ({
  getPermissionsAsync: vi.fn(),
  requestPermissionsAsync: vi.fn(),
  getDevicePushTokenAsync: vi.fn(),
  addPushTokenListener: vi.fn(() => ({ remove: vi.fn() })),
  setNotificationHandler: vi.fn(),
}));
const platform = vi.hoisted(() => ({ OS: "ios", select: () => () => "" }));
const device = vi.hoisted(() => ({ isDevice: false }));
const api = vi.hoisted(() => ({ registerPushEndpoint: vi.fn() }));

const pushCrypto = vi.hoisted(() => ({
  getPublicKey: vi.fn(() => Promise.resolve("dGVzdC1wdWJsaWMta2V5LWJhc2U2NA==")),
  getApsEnvironment: vi.fn((): string | null => null),
}));
vi.mock("react-native", () => ({
  Platform: platform,
  NativeModules: { SettingsManager: { settings: {} } },
}));
vi.mock("expo-modules-core", () => ({
  requireNativeModule: (name: string) => {
    if (name === "PushCrypto") return pushCrypto;
    throw new Error(`Module ${name} not found`);
  },
}));
vi.mock("expo-device", () => device);
vi.mock("expo-notifications", () => notifications);
vi.mock("./auth/api", () => api);

const { requestAndRegisterPush, listenForTokenChanges, configureForegroundNotifications } =
  await import("./push");

function session(serverDeviceId: string | null) {
  return {
    serverDeviceId: () => serverDeviceId,
    status: () => ({ kind: "enrolled" }),
    withAccessToken: async (_base: string, fn: (t: string) => Promise<void>) =>
      fn("access-token"),
  } as never;
}

let logs: string[];

function dev(value: boolean): void {
  (globalThis as Record<string, unknown>).__DEV__ = value;
}

/** Reads the environment the launch registration actually posted. */
async function registeredEnvironment(): Promise<string> {
  await requestAndRegisterPush(session("dev-1"), "http://c", fetch);
  return api.registerPushEndpoint.mock.calls[0][3].environment;
}

beforeEach(() => {
  logs = [];
  vi.spyOn(console, "log").mockImplementation((m) => void logs.push(String(m)));
  vi.spyOn(console, "warn").mockImplementation((m) => void logs.push(String(m)));
  platform.OS = "ios";
  device.isDevice = false;
  notifications.getPermissionsAsync.mockResolvedValue({ status: "granted" });
  notifications.getDevicePushTokenAsync.mockResolvedValue({ data: "a".repeat(64) });
  api.registerPushEndpoint.mockResolvedValue(undefined);
  pushCrypto.getApsEnvironment.mockReturnValue(null);
  // The bundler defines this on every real build; the node test
  // environment does not, and the fallback branch reads it.
  dev(false);
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.clearAllMocks();
});

/** Every refusal must say why. These paths return false identically, so
 * without a distinct reason a device log cannot tell them apart. */
describe("requestAndRegisterPush refusals", () => {
  it("says so when the platform is not iOS", async () => {
    platform.OS = "android";
    expect(await requestAndRegisterPush(session("dev-1"), "http://c", fetch)).toBe(false);
    expect(logs.join("\n")).toContain("iOS only");
  });

  it("says so when the device is not enrolled", async () => {
    expect(await requestAndRegisterPush(session(null), "http://c", fetch)).toBe(false);
    expect(logs.join("\n")).toContain("not enrolled");
  });

  it("names the permission status when permission is refused", async () => {
    notifications.getPermissionsAsync.mockResolvedValue({ status: "denied" });
    notifications.requestPermissionsAsync.mockResolvedValue({ status: "denied" });
    expect(await requestAndRegisterPush(session("dev-1"), "http://c", fetch)).toBe(false);
    expect(logs.join("\n")).toContain("permission is denied");
  });

  it("says so when APNs returns no token", async () => {
    notifications.getDevicePushTokenAsync.mockResolvedValue({ data: "" });
    expect(await requestAndRegisterPush(session("dev-1"), "http://c", fetch)).toBe(false);
    expect(logs.join("\n")).toContain("no token");
  });

  it("reports a registration that throws instead of swallowing it", async () => {
    api.registerPushEndpoint.mockRejectedValue(new Error("controller refused"));
    expect(await requestAndRegisterPush(session("dev-1"), "http://c", fetch)).toBe(false);
    expect(logs.join("\n")).toContain("registration failed");
  });
});

describe("requestAndRegisterPush success", () => {
  it("registers the native token and reports the environment", async () => {
    expect(await requestAndRegisterPush(session("dev-1"), "http://c", fetch)).toBe(true);
    expect(api.registerPushEndpoint).toHaveBeenCalledOnce();
    const registration = api.registerPushEndpoint.mock.calls[0][3];
    expect(registration.provider).toBeUndefined();
    expect(registration.token).toBe("a".repeat(64));
    // A simulator is always sandbox regardless of build configuration.
    expect(registration.environment).toBe("sandbox");
    expect(logs.join("\n")).toContain("registered with the controller as sandbox");
  });

  it("includes the HPKE public key when the native module is available", async () => {
    await requestAndRegisterPush(session("dev-1"), "http://c", fetch);
    const registration = api.registerPushEndpoint.mock.calls[0][3];
    expect(registration.publicKey).toBe("dGVzdC1wdWJsaWMta2V5LWJhc2U2NA==");
  });

  it("registers without a public key when the native module is absent", async () => {
    pushCrypto.getPublicKey.mockRejectedValueOnce(new Error("unavailable"));
    await requestAndRegisterPush(session("dev-1"), "http://c", fetch);
    const registration = api.registerPushEndpoint.mock.calls[0][3];
    expect(registration.publicKey).toBeUndefined();
  });

  it("never writes the device token to the log", async () => {
    await requestAndRegisterPush(session("dev-1"), "http://c", fetch);
    const written = logs.join("\n");
    expect(written).not.toContain("a".repeat(64));
    expect(written).toContain("64 chars");
  });
});

/** APNs accepts a token only at the host matching the `aps-environment`
 * the app was signed with, and answers the wrong host with
 * `BadDeviceToken`. The entitlement is therefore read from the signing
 * profile rather than inferred from the build configuration, which
 * disagrees with it whenever a Release build is installed over USB. */
describe("APNs environment detection", () => {
  it("reports sandbox for a development profile even in a Release build", async () => {
    device.isDevice = true;
    dev(false);
    pushCrypto.getApsEnvironment.mockReturnValue("development");
    expect(await registeredEnvironment()).toBe("sandbox");
  });

  it("reports production for a distribution profile", async () => {
    device.isDevice = true;
    dev(false);
    pushCrypto.getApsEnvironment.mockReturnValue("production");
    expect(await registeredEnvironment()).toBe("production");
  });

  it("reports sandbox on a simulator, which mints no entitlement to read", async () => {
    device.isDevice = false;
    dev(false);
    pushCrypto.getApsEnvironment.mockReturnValue("production");
    expect(await registeredEnvironment()).toBe("sandbox");
  });

  /** An App Store build carries no provisioning profile to read. */
  it("falls back to the build configuration when no profile is embedded", async () => {
    device.isDevice = true;
    dev(false);
    pushCrypto.getApsEnvironment.mockReturnValue(null);
    expect(await registeredEnvironment()).toBe("production");
  });

  it("falls back when the native module predates the entitlement reader", async () => {
    device.isDevice = true;
    dev(true);
    const absent = pushCrypto.getApsEnvironment;
    delete (pushCrypto as Partial<typeof pushCrypto>).getApsEnvironment;
    try {
      expect(await registeredEnvironment()).toBe("sandbox");
    } finally {
      pushCrypto.getApsEnvironment = absent;
    }
  });
});

/** The listener's subscription fires immediately with the current token,
 * which is the one the launch registration is already posting, so every
 * launch registered the same endpoint twice. */
describe("listenForTokenChanges", () => {
  /** Registers `token` through the launch path, then hands back the
   * listener the subscription was created with. */
  async function launchThenListen(token: string) {
    notifications.getDevicePushTokenAsync.mockResolvedValue({ data: token });
    await requestAndRegisterPush(session("dev-1"), "http://c", fetch);
    listenForTokenChanges(session("dev-1"), "http://c", fetch);
    const calls = notifications.addPushTokenListener.mock.calls;
    return calls[calls.length - 1][0] as (e: { data: string }) => void;
  }

  it("skips a token identical to the one just registered", async () => {
    const token = "b".repeat(64);
    const listener = await launchThenListen(token);
    api.registerPushEndpoint.mockClear();

    listener({ data: token });
    await vi.waitFor(() => expect(logs.join("\n")).toContain("token unchanged"));

    expect(api.registerPushEndpoint).not.toHaveBeenCalled();
  });

  it("registers a rotated token, because APNs may reissue at any time", async () => {
    const listener = await launchThenListen("c".repeat(64));
    api.registerPushEndpoint.mockClear();

    const rotated = "d".repeat(64);
    listener({ data: rotated });
    await vi.waitFor(() => expect(api.registerPushEndpoint).toHaveBeenCalledOnce());

    expect(api.registerPushEndpoint.mock.calls[0][3].token).toBe(rotated);
  });

  it("re-registers a token whose first attempt failed", async () => {
    const listener = await launchThenListen("e".repeat(64));
    const rotated = "f".repeat(64);
    api.registerPushEndpoint.mockClear();
    api.registerPushEndpoint.mockRejectedValueOnce(new Error("controller down"));

    listener({ data: rotated });
    await vi.waitFor(() => expect(logs.join("\n")).toContain("re-registration"));

    listener({ data: rotated });
    await vi.waitFor(() => expect(api.registerPushEndpoint).toHaveBeenCalledTimes(2));
  });
});

/** Without a handler iOS gives a foreground notification to the app and
 * shows nothing, so these assert the behaviour actually returned. */
describe("configureForegroundNotifications", () => {
  it("presents a notification that arrives while the app is open", async () => {
    configureForegroundNotifications();
    expect(notifications.setNotificationHandler).toHaveBeenCalledOnce();
    const handler = notifications.setNotificationHandler.mock.calls[0][0];
    const behavior = await handler.handleNotification();
    expect(behavior.shouldShowBanner).toBe(true);
    expect(behavior.shouldShowList).toBe(true);
    expect(behavior.shouldPlaySound).toBe(true);
  });

  it("leaves the badge alone because the app is already open", async () => {
    configureForegroundNotifications();
    const handler = notifications.setNotificationHandler.mock.calls[0][0];
    const behavior = await handler.handleNotification();
    expect(behavior.shouldSetBadge).toBe(false);
  });
});
