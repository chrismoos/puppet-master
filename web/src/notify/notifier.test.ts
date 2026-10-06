import { afterEach, describe, expect, it, vi } from "vitest";
import { Notifier } from "./notifier";


describe("Notifier tags", () => {
  afterEach(() => vi.unstubAllGlobals());

  function stubNotifications() {
    const created: FakeNotification[] = [];
    class FakeNotification {
      static permission = "granted";
      onclick: (() => void) | null = null;
      onclose: (() => void) | null = null;
      closed = false;
      constructor(
        readonly title: string,
        readonly options?: NotificationOptions,
      ) {
        created.push(this);
      }
      close(): void {
        this.closed = true;
        this.onclose?.();
      }
    }
    vi.stubGlobal("Notification", FakeNotification);
    return created;
  }

  it("keeps finish and needs-input alerts on distinct tags so they do not collapse", () => {
    const created = stubNotifications();

    const notifier = new Notifier();
    notifier.fire("blocked", undefined, "7");
    notifier.fire("done", "finished", "7", "finished");

    expect(created.map(({ options }) => options?.tag)).toEqual([
      "pm-needs-input-7-1",
      "pm-finished-7-2",
    ]);
    expect(created.map(({ closed }) => closed)).toEqual([false, false]);
  });

  it("shows a repeat alert as a new notification and closes the one it supersedes", () => {
    const created = stubNotifications();

    const notifier = new Notifier();
    notifier.fire("done", "finished", "7", "finished");
    notifier.fire("done", "finished", "7", "finished");

    const [first, second] = created;
    expect(first.options?.tag).not.toEqual(second.options?.tag);
    expect(first.closed).toBe(true);
    expect(second.closed).toBe(false);
  });

  it("leaves alone a notification the user already dismissed", () => {
    const created = stubNotifications();

    const notifier = new Notifier();
    notifier.fire("done", "finished", "7", "finished");
    created[0].close();
    const closeSpy = vi.spyOn(created[0], "close");
    notifier.fire("done", "finished", "7", "finished");

    expect(closeSpy).not.toHaveBeenCalled();
  });
});

describe("Notifier support", () => {
  afterEach(() => vi.unstubAllGlobals());

  class FakeNotification {
    static permission = "default";
    static requestPermission = vi.fn(async () => "granted" as NotificationPermission);
  }

  it("is supported on a secure origin with the API present", () => {
    vi.stubGlobal("Notification", FakeNotification);
    vi.stubGlobal("window", { isSecureContext: true });

    expect(new Notifier().support).toEqual({ supported: true });
  });

  it("reports an insecure origin instead of offering a request that would be denied", async () => {
    vi.stubGlobal("Notification", FakeNotification);
    vi.stubGlobal("window", { isSecureContext: false });

    const notifier = new Notifier();
    expect(notifier.support).toEqual({ supported: false, reason: "insecure-context" });
    expect(notifier.supported).toBe(false);
    expect(await notifier.requestPermission()).toBe("denied");
    expect(FakeNotification.requestPermission).not.toHaveBeenCalled();
  });

  it("reports a missing API as unavailable", () => {
    vi.stubGlobal("Notification", undefined);
    vi.stubGlobal("window", { isSecureContext: true });

    expect(new Notifier().support).toEqual({ supported: false, reason: "unavailable" });
  });
});
