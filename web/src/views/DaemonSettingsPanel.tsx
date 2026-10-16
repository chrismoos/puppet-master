import { useMemo, useState, type KeyboardEvent } from "react";
import {
  CATEGORY_DESCRIPTIONS,
  CATEGORY_ORDER,
  fetchChoices,
  isBooleanSetting,
  isNumericSetting,
  isSecretSetting,
  settingChoicesUrl,
  settingMetadata,
  settingOptions,
  type DaemonSetting,
  type FetchedChoice,
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
  /// Choices fetched from a provider, by setting key. Never fetched on
  /// load: it costs a credentialed round trip the user has to ask for.
  const [fetched, setFetched] = useState<Record<string, FetchedChoice[]>>({});
  const [fetching, setFetching] = useState<string | null>(null);
  const [fetchTrouble, setFetchTrouble] = useState<Record<string, string>>({});

  const loadChoices = async (setting: DaemonSetting, url: string) => {
    setFetching(setting.key);
    setFetchTrouble((current) => ({ ...current, [setting.key]: "" }));
    try {
      const choices = await fetchChoices(url);
      setFetched((current) => ({ ...current, [setting.key]: choices }));
    } catch (err) {
      // The provider's own reason, which names a missing permission
      // when a key cannot list what it can speak with.
      setFetchTrouble((current) => ({
        ...current,
        [setting.key]: err instanceof Error ? err.message : String(err),
      }));
    } finally {
      setFetching(null);
    }
  };

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

  /// A credential is written only when something was typed. An empty
  /// box means "leave the stored one alone", because the stored value is
  /// never shown and an empty commit would silently erase it.
  const commitSecretDraft = (setting: DaemonSetting) => {
    const typed = drafts[setting.key];
    if (typed === undefined || typed === "") return;
    onWriteSetting(setting.key, typed);
    setDrafts((current) => {
      const next = { ...current };
      delete next[setting.key];
      return next;
    });
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
              const secret = isSecretSetting(setting);
              const choices = settingOptions(setting);
              const choicesUrl = settingChoicesUrl(setting);
              const loaded = fetched[setting.key];
              const loadFailed = fetchTrouble[setting.key];
              const effectiveVal = drafts[setting.key] ?? setting.value;
              // A credential is never echoed, so the command cannot
              // carry one the way an ordinary setting's does.
              const command = secret
                ? `pm config set ${setting.key} <value>`
                : `pm config set ${setting.key} ${setting.value}`;

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
                    ) : choices.length > 0 ? (
                      <select
                        className="ui-input ui-w-md"
                        aria-label={meta.title}
                        value={effectiveVal}
                        onChange={(e) => onWriteSetting(setting.key, e.target.value || null)}
                      >
                        {/* The stored value is always offered, even when
                            it is not one of the choices: a select whose
                            value is absent displays a different one,
                            which would show a setting as something it
                            is not. */}
                        {(choices.includes(effectiveVal)
                          ? choices
                          : [effectiveVal, ...choices]
                        ).map((choice) => (
                          <option key={choice} value={choice}>
                            {choice === "" ? "not set" : choice}
                          </option>
                        ))}
                      </select>
                    ) : secret ? (
                      <span className="set-secret">
                        <input
                          type="password"
                          className="ui-input ui-w-md"
                          autoComplete="new-password"
                          // Never the stored value, which the daemon does
                          // not return: only what is being typed now.
                          value={drafts[setting.key] ?? ""}
                          placeholder={setting.set ? "stored" : "not set"}
                          aria-label={meta.title}
                          onChange={(e) => updateDraft(setting.key, e.target.value)}
                          onBlur={() => commitSecretDraft(setting)}
                          onKeyDown={(e) => handleStringKeyDown(e, setting)}
                        />
                        {setting.set && (
                          <button
                            type="button"
                            className="btn"
                            aria-label={`Clear ${meta.title}`}
                            onClick={() => {
                              updateDraft(setting.key, "");
                              onWriteSetting(setting.key, null);
                            }}
                          >
                            Clear
                          </button>
                        )}
                      </span>
                    ) : (
                      <span className="set-choosable">
                        <input
                          type="text"
                          className="ui-input ui-w-md"
                          list={loaded ? `${setting.key}-choices` : undefined}
                          value={effectiveVal}
                          aria-label={meta.title}
                          onChange={(e) => updateDraft(setting.key, e.target.value)}
                          onBlur={() => commitStringDraft(setting)}
                          onKeyDown={(e) => handleStringKeyDown(e, setting)}
                        />
                        {/* The field stays typable whatever happens to
                            the fetch: a model newer than the provider's
                            own listing is still selectable, and a key
                            that cannot list does not lock the setting. */}
                        {choicesUrl && (
                          <button
                            type="button"
                            className="btn"
                            title={`List the choices for ${meta.title}`}
                            aria-label={`List choices for ${meta.title}`}
                            disabled={fetching === setting.key}
                            onClick={() => void loadChoices(setting, choicesUrl)}
                          >
                            {fetching === setting.key ? "..." : "List"}
                          </button>
                        )}
                        {loaded && (
                          <>
                            <datalist id={`${setting.key}-choices`}>
                              {loaded.map((choice) => (
                                <option key={choice.id} value={choice.id}>
                                  {choice.name}
                                </option>
                              ))}
                            </datalist>
                            <select
                              className="ui-input ui-w-sm"
                              aria-label={`Choose ${meta.title}`}
                              value=""
                              onChange={(e) => {
                                if (!e.target.value) return;
                                updateDraft(setting.key, e.target.value);
                                onWriteSetting(setting.key, e.target.value);
                              }}
                            >
                              <option value="">{loaded.length} available</option>
                              {loaded.map((choice) => (
                                <option key={choice.id} value={choice.id}>
                                  {choice.name}
                                </option>
                              ))}
                            </select>
                          </>
                        )}
                        {loadFailed && <span className="set-trouble">{loadFailed}</span>}
                      </span>
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
