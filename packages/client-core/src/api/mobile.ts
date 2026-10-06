/** Device ids stay strings end to end so u64 values keep full precision. */
export interface MobileDevice {
  id: string;
  name: string;
  platform: string;
  appInstallationId: string;
  createdAtUnixMs: number;
  lastSeenAtUnixMs: number | null;
  revokedAtUnixMs: number | null;
}
