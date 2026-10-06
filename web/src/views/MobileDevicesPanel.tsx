import { useEffect, useState } from "react";
import { formatAgo } from "@puppet-master/client-core/format";
import { fetchMobileDevices, revokeMobileDevice, type MobileDevice } from "../api/mobile";
import { SettingsError, SettingsPageHead, errorMessage } from "./settingsParts";

const MS_PER_DAY = 86_400_000;
const DATE_FORMAT: Intl.DateTimeFormatOptions = { year: "numeric", month: "short", day: "numeric" };

function formatDay(unixMs: number): string {
  return new Date(unixMs).toLocaleDateString(undefined, DATE_FORMAT);
}

/**
 * When a device last reached the controller. Activity inside the last day
 * reads as an age and counts as recent; anything older reads as a date.
 */
export function mobileDeviceLastSeen(
  device: Pick<MobileDevice, "lastSeenAtUnixMs">,
  now: number,
): { label: string; recent: boolean } {
  if (device.lastSeenAtUnixMs === null) return { label: "never", recent: false };
  const age = Math.max(0, now - device.lastSeenAtUnixMs);
  if (age < MS_PER_DAY) return { label: `${formatAgo(age)} ago`, recent: true };
  return { label: formatDay(device.lastSeenAtUnixMs), recent: false };
}

const PLATFORM_LABELS: Readonly<Record<string, string>> = {
  ios: "iOS",
  android: "Android",
  cli: "Command line",
};

/** What a device's reported platform reads as; an unknown one is shown as sent. */
export function devicePlatformLabel(platform: string): string {
  return PLATFORM_LABELS[platform.toLowerCase()] ?? platform;
}

export function MobileDevicesPanel() {
  const [devices, setDevices] = useState<MobileDevice[]>([]);
  const [error, setError] = useState<string | null>(null);

  const reload = () => {
    fetchMobileDevices()
      .then(setDevices)
      .catch((err: unknown) => setError(errorMessage(err)));
  };

  useEffect(reload, []);

  const revoke = (device: MobileDevice) => {
    setError(null);
    revokeMobileDevice(device.id)
      .then(reload)
      .catch((err: unknown) => setError(errorMessage(err)));
  };

  const now = Date.now();

  return (
    <section className="set-page" aria-labelledby="settings-page-title">
      <SettingsPageHead
        title="Devices"
        description="Phones and command-line logins signed in to this controller. To add one, sign in from the Puppet Master app or with pm login, using your username and password."
      />
      <SettingsError message={error} onDismiss={() => setError(null)} />
      {devices.length === 0 ? (
        <div className="ui-empty">
          <b>No devices enrolled</b>
          <p>Sign in from the app or with pm login, using your username and password, to enroll one.</p>
        </div>
      ) : (
        <div className="ui-list set-table-wrap">
          <table className="ui-grid set-devices">
            <thead>
              <tr>
                <th>Device</th>
                <th>Device ID</th>
                <th>Enrolled</th>
                <th>Last seen</th>
                <th><span className="visually-hidden">Actions</span></th>
              </tr>
            </thead>
            <tbody>
              {devices.map((device) => {
                const seen = mobileDeviceLastSeen(device, now);
                return (
                  <tr className="device-row" key={device.id} data-device-id={device.id}>
                    <td data-label="Device">
                      <b className="device-name">{device.name || "Unnamed device"}</b>
                      {device.platform && <div className="sub device-platform">{devicePlatformLabel(device.platform)}</div>}
                    </td>
                    <td data-label="Device ID">
                      <code className="ui-key device-install-id" title="Device ID, shown in the app under Settings">
                        {device.appInstallationId}
                      </code>
                    </td>
                    <td data-label="Enrolled" className="sub" title={new Date(device.createdAtUnixMs).toLocaleString()}>
                      <span>{formatDay(device.createdAtUnixMs)}</span>
                    </td>
                    <td
                      data-label="Last seen"
                      className={seen.recent ? undefined : "sub"}
                      title={device.lastSeenAtUnixMs === null ? undefined : new Date(device.lastSeenAtUnixMs).toLocaleString()}
                    >
                      <span>{seen.recent && <span className="ui-dot ok" aria-hidden="true" />} {seen.label}</span>
                    </td>
                    <td className="right">
                      <button type="button" className="btn btn-quiet btn-quiet-danger" onClick={() => revoke(device)}>
                        Revoke
                      </button>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
      <p className="ui-hint set-below">
        Revoking signs the device out at once. Token lifetimes are under{" "}
        <a href="#/settings/daemon">Daemon</a>.
      </p>
    </section>
  );
}
