import { useEffect, useState, type FormEvent } from "react";
import type { ModelProfile, ModelProfileEndpoint } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { ModelDialect } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { agentLabel } from "@puppet-master/client-core/state/agent";
import type { AgentDialects } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import {
  DIALECTS,
  agentsForDialect,
  backgroundModelApplies,
  coveredAgents,
  dialectLabel,
} from "@puppet-master/client-core/state/modelProfile";
import { useAppState, useClient } from "../state/hooks";
import { SettingsError, SettingsPageHead } from "./settingsParts";

interface EndpointDraft {
  model: string;
  baseUrl: string;
  backgroundModel: string;
}

const EMPTY_DRAFT: EndpointDraft = { model: "", baseUrl: "", backgroundModel: "" };

function draftOf(endpoint: ModelProfileEndpoint | undefined): EndpointDraft {
  if (!endpoint) return EMPTY_DRAFT;
  return {
    model: endpoint.model,
    baseUrl: endpoint.baseUrl,
    backgroundModel: endpoint.backgroundModel,
  };
}

export function ModelProfilesPanel() {
  const state = useAppState();
  const client = useClient();
  const profiles = [...state.modelProfiles.values()].sort((a, b) => a.name.localeCompare(b.name));

  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [newName, setNewName] = useState("");
  const [newKey, setNewKey] = useState("");

  const run = async (action: () => Promise<unknown>, after?: () => void) => {
    setBusy(true);
    setError(null);
    try {
      await action();
      after?.();
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  const create = (event: FormEvent) => {
    event.preventDefault();
    void run(
      () => client.createModelProfile(newName.trim(), newKey.trim() || undefined),
      () => {
        setNewName("");
        setNewKey("");
      },
    );
  };

  return (
    <section className="set-page model-profiles" aria-label="Model profiles">
      <SettingsPageHead
        title="Model profiles"
        description="A profile is one provider account: a credential plus one endpoint per API dialect. Attach it to a bucket or project, or pick it for a single spawn."
      />
      <SettingsError message={error} onDismiss={() => setError(null)} />

      {profiles.length === 0 ? (
        <div className="ui-empty">
          <b>No model profiles yet</b>
          <p>
            Without one, every agent uses its own account and endpoint. Add a profile to route
            agents through a different provider, a proxy, or a local model.
          </p>
        </div>
      ) : (
        <div className="ui-list set-profiles">
          {profiles.map((profile) => {
            const agents = coveredAgents(profile, state.agentDialects).map(agentLabel);
            const open = selectedId === profile.id.toString();
            return (
              <div className="model-profile" key={profile.id.toString()} data-profile-id={profile.id.toString()}>
                <div className="model-profile-head">
                  <button
                    type="button"
                    className="catalog-name"
                    aria-expanded={open}
                    onClick={() => setSelectedId(open ? null : profile.id.toString())}
                  >
                    <b>{profile.name}</b>
                    <small>
                      {agents.length ? `covers ${agents.join(", ")}` : "covers no agent yet"}
                      {" · "}
                      {profile.keySet ? "API key set" : "no API key"}
                    </small>
                  </button>
                  <button
                    type="button"
                    className="btn btn-quiet btn-quiet-danger"
                    disabled={busy}
                    onClick={() => void run(() => client.deleteModelProfile(profile.id))}
                  >
                    Delete
                  </button>
                </div>
                {open && (
                  <ProfileEditor
                    profile={profile}
                    busy={busy}
                    run={run}
                    agents={agents}
                    agentDialects={state.agentDialects}
                  />
                )}
              </div>
            );
          })}
        </div>
      )}

      <div className="ui-sect">
        <form className="ui-form-card set-narrow-card" aria-labelledby="new-profile-title" onSubmit={create}>
          <header><h3 id="new-profile-title">New profile</h3></header>
          <div className="body">
            <div className="ui-field">
              <label htmlFor="new-profile-name">Name</label>
              <input
                id="new-profile-name"
                className="ui-input ui-w-md"
                value={newName}
                onChange={(event) => setNewName(event.target.value)}
                placeholder="team-anthropic"
                required
              />
            </div>
            <div className="ui-field">
              <label htmlFor="new-profile-key">API key</label>
              <input
                id="new-profile-key"
                className="ui-input"
                type="password"
                autoComplete="off"
                aria-describedby="new-profile-key-hint"
                value={newKey}
                onChange={(event) => setNewKey(event.target.value)}
                placeholder="Optional now, required before setting a base URL"
              />
              <span className="ui-hint" id="new-profile-key-hint">
                Write-only. It can be replaced later but never read back.
              </span>
            </div>
          </div>
          <footer>
            <button type="submit" className="btn btn-primary btn-lg" disabled={busy || !newName.trim()}>
              Create profile
            </button>
            <span className="ui-hint">Endpoints and models are set after you create it.</span>
          </footer>
        </form>
      </div>
    </section>
  );
}

function ProfileEditor({
  profile,
  busy,
  run,
  agents,
  agentDialects,
}: {
  profile: ModelProfile;
  busy: boolean;
  run: (action: () => Promise<unknown>, after?: () => void) => Promise<void>;
  agents: string[];
  agentDialects: readonly AgentDialects[];
}) {
  const client = useClient();
  const [name, setName] = useState(profile.name);
  const [key, setKey] = useState("");

  useEffect(() => {
    setName(profile.name);
    setKey("");
  }, [profile.id, profile.name]);

  return (
    <div className="model-profile-body">
      <div className="model-profile-identity">
        <label className="field">
          <span className="field-label">name</span>
          <input value={name} onChange={(event) => setName(event.target.value)} />
        </label>
        <button
          type="button"
          className="btn"
          disabled={busy || !name.trim() || name === profile.name}
          onClick={() => void run(() => client.updateModelProfile(profile.id, { name: name.trim() }))}
        >
          Rename
        </button>
        <label className="field">
          <span className="field-label">replace API key</span>
          <input
            type="password"
            autoComplete="off"
            value={key}
            onChange={(event) => setKey(event.target.value)}
            placeholder={profile.keySet ? "key set — enter a new one to replace" : "no key set"}
          />
        </label>
        <button
          type="button"
          className="btn"
          disabled={busy || !key.trim()}
          onClick={() => void run(() => client.updateModelProfile(profile.id, { apiKey: key.trim() }), () => setKey(""))}
        >
          Replace key
        </button>
        {profile.keySet && (
          <button
            type="button"
            className="btn"
            disabled={busy}
            onClick={() => void run(() => client.updateModelProfile(profile.id, { clearApiKey: true }))}
          >
            Clear key
          </button>
        )}
      </div>
      <p className="muted-line">Covers {agents.length ? agents.join(", ") : "no agent"}.</p>
      {DIALECTS.map((dialect) => (
        <EndpointEditor
          key={dialect}
          profile={profile}
          dialect={dialect}
          busy={busy}
          run={run}
          agentDialects={agentDialects}
        />
      ))}
    </div>
  );
}

function EndpointEditor({
  profile,
  dialect,
  busy,
  run,
  agentDialects,
}: {
  profile: ModelProfile;
  dialect: ModelDialect;
  busy: boolean;
  run: (action: () => Promise<unknown>, after?: () => void) => Promise<void>;
  agentDialects: readonly AgentDialects[];
}) {
  const client = useClient();
  const stored = profile.endpoints.find((endpoint) => endpoint.dialect === dialect);
  const [draft, setDraft] = useState<EndpointDraft>(draftOf(stored));
  // Every agent that runs this dialect may ignore the background model.
  // Say so rather than accepting a value the adapter drops.
  const backgroundApplies = backgroundModelApplies(dialect, agentDialects);
  const dialectAgents = agentsForDialect(dialect, agentDialects)
    .map((entry) => agentLabel(entry.agent));

  useEffect(() => {
    setDraft(draftOf(stored));
  }, [stored?.model, stored?.baseUrl, stored?.backgroundModel, stored]);

  return (
    <fieldset className="model-endpoint" data-dialect={dialectLabel(dialect)}>
      <legend>{dialectLabel(dialect)}</legend>
      <label className="field">
        <span className="field-label">model</span>
        <input
          value={draft.model}
          onChange={(event) => setDraft({ ...draft, model: event.target.value })}
          placeholder="model the main reasoning loop uses"
        />
      </label>
      <label className="field">
        <span className="field-label">base URL</span>
        <input
          value={draft.baseUrl}
          onChange={(event) => setDraft({ ...draft, baseUrl: event.target.value })}
          placeholder="empty uses the agent's own endpoint and account"
        />
      </label>
      <label className="field">
        <span className="field-label">background model</span>
        <input
          value={backgroundApplies ? draft.backgroundModel : ""}
          disabled={!backgroundApplies}
          onChange={(event) => setDraft({ ...draft, backgroundModel: event.target.value })}
          placeholder={backgroundApplies
            ? "cheaper model for auxiliary work off the main loop"
            : "not applicable"}
        />
        {!backgroundApplies && (
          <span className="field-note" data-testid={`background-na-${dialectLabel(dialect)}`}>
            {dialectAgents.join(", ") || "This agent"} {dialectAgents.length > 1 ? "have" : "has"}
            {" "}no small/fast model setting, so a background model would be ignored.
          </span>
        )}
      </label>
      <div className="model-endpoint-actions">
        <button
          type="button"
          className="btn btn-primary"
          disabled={busy || !draft.model.trim()}
          onClick={() =>
            void run(() =>
              client.setModelProfileEndpoint(
                profile.id,
                dialect,
                draft.model.trim(),
                draft.baseUrl.trim(),
                draft.backgroundModel.trim(),
              ),
            )
          }
        >
          {stored ? "Save endpoint" : "Add endpoint"}
        </button>
        {stored && (
          <button
            type="button"
            className="btn btn-danger"
            disabled={busy}
            onClick={() => void run(() => client.deleteModelProfileEndpoint(profile.id, dialect))}
          >
            Remove
          </button>
        )}
      </div>
    </fieldset>
  );
}
