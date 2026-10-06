export { type MobileDevice } from "@puppet-master/client-core/api/mobile";
import type { MobileDevice } from "@puppet-master/client-core/api/mobile";
import { authedFetch } from "./token";

const HTTP_UNAUTHORIZED = 401;

async function ensureOk(res: Response, what: string): Promise<void> {
  if (res.status === HTTP_UNAUTHORIZED) throw new Error("not authenticated");
  if (!res.ok) {
    const body = (await res.json().catch(() => null)) as { error?: string } | null;
    throw new Error(body?.error || `${what} (${res.status})`);
  }
}

export async function fetchMobileDevices(): Promise<MobileDevice[]> {
  const res = await authedFetch("/api/mobile/devices");
  await ensureOk(res, "listing devices failed");
  const body = (await res.json()) as { devices: MobileDevice[] };
  return body.devices;
}

export async function revokeMobileDevice(deviceId: string): Promise<void> {
  const res = await authedFetch(`/api/mobile/devices/${deviceId}`, { method: "DELETE" });
  await ensureOk(res, "revoking the device failed");
}
