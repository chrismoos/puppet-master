import { useEffect, useRef, useState } from "react";
import { fetchVersion, type BuildInfo } from "../api/auth";
import { fetchSettings, updateSetting, type DaemonSetting } from "../api/settings";
import {
  SETTINGS_GROUPS,
  navigate,
  settingsRoutePath,
  type SettingsCatalog,
  type SettingsSection,
} from "../router";
import type { ConnectionView } from "@puppet-master/client-core/router";
import { useAppState } from "../state/hooks";
import {
  AppearanceSettings,
  NotificationSettings,
  PasswordSettings,
  TerminalThemeSettings,
} from "./AccountSettings";
import { SettingsConnections } from "./ConnectionsManage";
import { DaemonSettingsPanel } from "./DaemonSettingsPanel";
import { InstructionsPanel } from "./InstructionsPanel";
import { MobileDevicesPanel } from "./MobileDevicesPanel";
import { ModelProfilesPanel } from "./ModelProfilesPanel";
import { ProjectsCatalog } from "./ProjectsCatalog";
import { SETTINGS_PAGE_TITLE_ID, SettingsError, errorMessage } from "./settingsParts";
import { controllerBuildVersion, WorkersPanel } from "./WorkersPanel";
import "./Settings.css";

export const SETTINGS_LABELS: Readonly<Record<SettingsSection, string>> = {
  appearance: "Appearance",
  "terminal-theme": "Terminal theme",
  notifications: "Notifications",
  password: "Password",
  projects: "Projects",
  connections: "Connections",
  models: "Model profiles",
  instructions: "Instructions",
  workers: "Workers",
  mobile: "Devices",
  daemon: "Daemon",
};

/** Names the channel a controller follows, when it is not stable. */
export function channelNote(build: Pick<BuildInfo, "channel"> | null): string {
  if (!build || !build.channel || build.channel === "stable") return "";
  return ` on the ${build.channel} channel`;
}

export function SettingsPage({
  username,
  section,
  catalog,
  onBack,
  preselectBucket,
  onPreselectConsumed,
  selectedBucketId,
  selectedProjectId,
  connection,
}: {
  username: string;
  section: SettingsSection;
  catalog?: SettingsCatalog;
  onBack: () => void;
  preselectBucket?: string | null;
  onPreselectConsumed?: () => void;
  selectedBucketId?: string;
  selectedProjectId?: string;
  /** Where the reader is inside the Connections page. */
  connection?: ConnectionView;
}) {
  const state = useAppState();
  const mainRef = useRef<HTMLElement>(null);
  const [build, setBuild] = useState<BuildInfo | null>(null);

  useEffect(() => {
    let live = true;
    void fetchVersion().then((b) => {
      if (live) setBuild(b);
    });
    return () => {
      live = false;
    };
  }, []);

  useEffect(() => {
    if (section === "projects" && (selectedBucketId || selectedProjectId)) return;
    const main = mainRef.current;
    if (!main) return;
    main.scrollTop = 0;
    (main.querySelector<HTMLElement>(`#${SETTINGS_PAGE_TITLE_ID}`) ?? main).focus();
  }, [section, selectedBucketId, selectedProjectId]);

  const counts: Partial<Record<SettingsSection, number>> = {
    projects: state.projects.size,
    models: state.modelProfiles.size,
    workers: state.workers.size,
  };
  const version = controllerBuildVersion(build);

  return (
    <div className="set-shell">
      <nav className="set-nav" aria-label="Settings">
        <div>
          <button type="button" className="set-back" onClick={onBack}>
            <span aria-hidden="true">‹</span> Back to sessions
          </button>
          <h1>Settings</h1>
        </div>
        {SETTINGS_GROUPS.map((group) => (
          <div key={group.label} role="group" aria-label={group.label}>
            <div className="set-nav-group" aria-hidden="true">{group.label}</div>
            {group.sections.map((item) => (
              <a
                key={item}
                className="set-nav-item"
                href={`#${settingsRoutePath(item)}`}
                aria-current={section === item ? "page" : undefined}
              >
                <span>{SETTINGS_LABELS[item]}</span>
                {counts[item] !== undefined && <span className="count ui-num">{counts[item]}</span>}
              </a>
            ))}
          </div>
        ))}
        <div className="set-nav-foot">
          Signed in as {username}
          {version && <><br />pm {version}{channelNote(build)}</>}
        </div>
      </nav>
      <div className="set-nav-mobile">
        <button type="button" className="btn set-back-mobile" onClick={onBack}>
          <span aria-hidden="true">‹</span> Sessions
        </button>
        <select
          className="ui-select"
          aria-label="Settings page"
          value={section}
          onChange={(event) => navigate(settingsRoutePath(event.target.value as SettingsSection))}
        >
          {SETTINGS_GROUPS.map((group) => (
            <optgroup key={group.label} label={group.label}>
              {group.sections.map((item) => (
                <option key={item} value={item}>{SETTINGS_LABELS[item]}</option>
              ))}
            </optgroup>
          ))}
        </select>
      </div>
      <main ref={mainRef} className="set-main" data-section={section} tabIndex={-1}>
        {section === "appearance" && <AppearanceSettings />}
        {section === "terminal-theme" && <TerminalThemeSettings />}
        {section === "notifications" && <NotificationSettings />}
        {section === "password" && <PasswordSettings />}
        {section === "projects" && (
          <ProjectsCatalog
            catalog={catalog}
            selectedBucketId={selectedBucketId}
            selectedProjectId={selectedProjectId}
            preselectBucket={preselectBucket}
            onPreselectConsumed={onPreselectConsumed}
          />
        )}
        {section === "connections" && (
          <section aria-label="Connections">
            <SettingsConnections view={connection} />
          </section>
        )}
        {section === "models" && <ModelProfilesPanel />}
        {section === "instructions" && <InstructionsPanel />}
        {section === "workers" && <WorkersPanel />}
        {section === "mobile" && <MobileDevicesPanel />}
        {section === "daemon" && <DaemonSettings />}
      </main>
    </div>
  );
}

function DaemonSettings() {
  const [settings, setSettings] = useState<DaemonSetting[]>([]);
  const [error, setError] = useState<string | null>(null);

  const reload = () => {
    fetchSettings()
      .then(setSettings)
      .catch((err: unknown) => setError(errorMessage(err)));
  };

  useEffect(reload, []);

  const write = (key: string, value: string | null) => {
    setError(null);
    updateSetting(key, value)
      .then(reload)
      .catch((err: unknown) => setError(errorMessage(err)));
  };

  return (
    <>
      <SettingsError message={error} onDismiss={() => setError(null)} />
      <DaemonSettingsPanel settings={settings} onWriteSetting={write} />
    </>
  );
}
