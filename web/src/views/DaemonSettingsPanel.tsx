import { useMemo, useState, type KeyboardEvent } from "react";
import {
  CATEGORY_DESCRIPTIONS,
  CATEGORY_ORDER,
  isBooleanSetting,
  isNumericSetting,
  settingMetadata,
  type DaemonSetting,
  type SettingMeta,
} from "../api/settings";
import { copyTextToClipboard } from "../clipboard";
import { SettingsPageHead } from "./settingsParts";

export interface DaemonSettingsPanelProps {
  settings: DaemonSetting[];
  onWriteSetting: (key: string, value: string | null) => void;
}

const COPIED_TIMEOUT_MS = 1_500;

export function DaemonSettingsPanel({ settings, onWriteSetting }: DaemonSettingsPanelProps) {
  const [query, setQuery] = useState("");
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [copiedKey, setCopiedKey] = useState<string | null>(null);

  const categorized = useMemo(() => {
    const q = query.trim().toLowerCase();
    const map = new Map<string, DaemonSetting[]>();

    for (const setting of settings) {
      const meta = settingMetadata(setting.key);
      if (q) {
        const matches =
          setting.key.toLowerCase().includes(q) ||
          meta.title.toLowerCase().includes(q) ||
          setting.description.toLowerCase().includes(q);
        if (!matches) continue;
      }
      const list = map.get(meta.category) ?? [];
      list.push(setting);
      map.set(meta.category, list);
    }

    const ordered: Array<{ category: string; description?: string; items: DaemonSetting[] }> = [];
    for (const cat of CATEGORY_ORDER) {
      const items = map.get(cat);
      if (items && items.length > 0) {
        ordered.push({ category: cat, description: CATEGORY_DESCRIPTIONS[cat], items });
        map.delete(cat);
      }
    }
    for (const [category, items] of map.entries()) {
      if (items.length > 0) {
        ordered.push({ category, description: CATEGORY_DESCRIPTIONS[category], items });
      }
    }
    return ordered;
  }, [settings, query]);

  const updateDraft = (key: string, value: string) => {
    setDrafts((prev) => ({ ...prev, [key]: value }));
  };

  const commitDraft = (setting: DaemonSetting, meta: SettingMeta) => {
    const raw = drafts[setting.key];
    if (raw === undefined) return;
    const trimmed = raw.trim();
    if (!/^\d+$/.test(trimmed)) {
      setDrafts((prev) => {
        const next = { ...prev };
        delete next[setting.key];
        return next;
      });
      return;
    }
    let parsed = Number.parseInt(trimmed, 10);
    if (meta.min !== undefined && parsed < meta.min) parsed = meta.min;
    if (meta.max !== undefined && parsed > meta.max) parsed = meta.max;
    const normalized = String(parsed);
    setDrafts((prev) => ({ ...prev, [setting.key]: normalized }));
    if (normalized !== setting.value) {
      onWriteSetting(setting.key, normalized);
    }
  };

  const handleNumericKeyDown = (
    e: KeyboardEvent<HTMLInputElement>,
    setting: DaemonSetting,
  ) => {
    if (e.key === "Enter") {
      e.currentTarget.blur();
    } else if (e.key === "Escape") {
      setDrafts((prev) => {
        const next = { ...prev };
        delete next[setting.key];
        return next;
      });
      e.currentTarget.blur();
    }
  };

  const step = (setting: DaemonSetting, meta: SettingMeta, direction: -1 | 1) => {
    const currentStr = drafts[setting.key] ?? setting.value;
    const current = Number.parseInt(currentStr, 10);
    const base = Number.isNaN(current) ? meta.min : current;
    let next = base + direction * meta.step;
    if (meta.min !== undefined && next < meta.min) next = meta.min;
    if (meta.max !== undefined && next > meta.max) next = meta.max;
    const nextStr = String(next);
    setDrafts((prev) => ({ ...prev, [setting.key]: nextStr }));
    if (nextStr !== setting.value) {
      onWriteSetting(setting.key, nextStr);
    }
  };

  const commitStringDraft = (setting: DaemonSetting) => {
    const raw = drafts[setting.key];
    if (raw === undefined) return;
    if (raw !== setting.value) {
      onWriteSetting(setting.key, raw);
    }
  };

  const handleStringKeyDown = (e: KeyboardEvent<HTMLInputElement>, setting: DaemonSetting) => {
    if (e.key === "Enter") {
      e.currentTarget.blur();
    } else if (e.key === "Escape") {
      setDrafts((prev) => {
        const next = { ...prev };
        delete next[setting.key];
        return next;
      });
      e.currentTarget.blur();
    }
  };

  const copyCli = async (setting: DaemonSetting) => {
    const cmd = `pm config set ${setting.key} ${setting.value}`;
    try {
      await copyTextToClipboard(cmd);
      setCopiedKey(setting.key);
      setTimeout(() => setCopiedKey(null), COPIED_TIMEOUT_MS);
    } catch {
      // Ignore clipboard refusal in environments without permissions.
    }
  };

  return (
    <section className="set-page" aria-label="Daemon settings">
      <SettingsPageHead
        title="Daemon"
        description={<>
          Controller-wide defaults and limits. A change applies to sessions spawned after it, and
          each one is also a <code>pm config set</code> key.
        </>}
        actions={
          <input
            type="search"
            className="ui-input ui-w-sm"
            placeholder="Filter settings"
            aria-label="Filter daemon settings"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        }
      />

      {categorized.length === 0 && query.trim() !== "" && (
        <div className="ui-empty">
          <b>No setting matches that filter</b>
          <p>Filter matches a setting's name, its description, or its config key.</p>
          <div>
            <button type="button" className="btn" onClick={() => setQuery("")}>
              Clear filter
            </button>
          </div>
        </div>
      )}
      {categorized.map(({ category, description, items }) => (
        <div className="ui-sect" key={category} data-category={category}>
          <div className="ui-sect-head">
            <h3>{category}</h3>
            {description && <span>{description}</span>}
          </div>
          <div className="ui-list">
            {items.map((setting) => {
              const meta = settingMetadata(setting.key);
              const isBool = isBooleanSetting(setting);
              const isNum = isNumericSetting(setting);
              const effectiveVal = drafts[setting.key] ?? setting.value;
              const command = `pm config set ${setting.key} ${setting.value}`;

              return (
                <div
                  className={`ui-row set-setting${setting.set ? " modified" : ""}`}
                  key={setting.key}
                  data-key={setting.key}
                >
                  <div className="main">
                    <div className="title">{meta.title}</div>
                    <div className="desc">{setting.description}</div>
                    <div className="ui-key">
                      <button
                        type="button"
                        onClick={() => void copyCli(setting)}
                        title={`Copy: ${command}`}
                        aria-label={`Copy command: ${command}`}
                      >
                        {copiedKey === setting.key ? "copied" : setting.key}
                      </button>
                      {setting.set && (
                        <span className="mod-note">
                          {" "}· changed from {setting.default || "empty"} ·{" "}
                          <button
                            type="button"
                            title={`Back to the default (${setting.default})`}
                            aria-label={`Reset ${meta.title}`}
                            onClick={() => onWriteSetting(setting.key, null)}
                          >
                            reset
                          </button>
                        </span>
                      )}
                    </div>
                  </div>

                  <div className="ctl">
                    {isBool ? (
                      <label className="ui-switch" title={`Toggle ${meta.title}`}>
                        <input
                          type="checkbox"
                          aria-label={meta.title}
                          checked={setting.value === "true"}
                          onChange={(e) =>
                            onWriteSetting(setting.key, e.target.checked ? "true" : "false")
                          }
                        />
                        <span />
                      </label>
                    ) : isNum ? (
                      <span className="ui-stepper">
                        <span className="box">
                          <button
                            type="button"
                            aria-label={`Decrease ${meta.title}`}
                            onClick={() => step(setting, meta, -1)}
                          >
                            −
                          </button>
                          <input
                            type="text"
                            inputMode="numeric"
                            pattern="[0-9]*"
                            value={effectiveVal}
                            aria-label={meta.title}
                            onChange={(e) => updateDraft(setting.key, e.target.value)}
                            onBlur={() => commitDraft(setting, meta)}
                            onKeyDown={(e) => handleNumericKeyDown(e, setting)}
                          />
                          <button
                            type="button"
                            aria-label={`Increase ${meta.title}`}
                            onClick={() => step(setting, meta, 1)}
                          >
                            +
                          </button>
                        </span>
                        {meta.unit}
                      </span>
                    ) : (
                      <input
                        type="text"
                        className="ui-input ui-w-md"
                        value={effectiveVal}
                        aria-label={meta.title}
                        onChange={(e) => updateDraft(setting.key, e.target.value)}
                        onBlur={() => commitStringDraft(setting)}
                        onKeyDown={(e) => handleStringKeyDown(e, setting)}
                      />
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      ))}
    </section>
  );
}
