import { useEffect, useState } from "react";
import { fetchVersion, type BuildInfo } from "../api/auth";
import { checkedAgo, releaseStateNote, UpdateInstructions } from "../components/UpdateNotice";
import { SettingsPageHead } from "./settingsParts";

/** The build this controller runs, and the published build it is behind. */
export function AboutPanel() {
  // Undefined until the daemon answers: an unanswered read is not the
  // same as a refusal, and the refusal is what the page says out loud.
  const [build, setBuild] = useState<BuildInfo | null | undefined>(undefined);

  useEffect(() => {
    let live = true;
    void fetchVersion().then((info) => {
      if (live) setBuild(info);
    });
    return () => {
      live = false;
    };
  }, []);

  return (
    <section className="set-page" aria-label="About this controller">
      <SettingsPageHead
        title="About"
        description={<>
          The build this controller runs, and the newest one published on the channel it follows.
        </>}
      />
      {build === null && (
        <div className="ui-empty">
          <b>The controller did not answer</b>
          <p>Its build is read from <code>/api/version</code>, which this page could not reach.</p>
        </div>
      )}
      {build && <AboutBuild build={build} />}
    </section>
  );
}

function AboutBuild({ build }: { build: BuildInfo }) {
  const checked = checkedAgo(build, Date.now());
  const behind = build.updateAvailable !== null;

  return (
    <div className="ui-sect about-build">
      <div className="ui-sect-head">
        <h3>Build</h3>
        {behind && <span className="badge update-badge">update available</span>}
      </div>
      <dl className="about-facts">
        <dt>Running</dt>
        <dd>{build.gitRev ? `${build.version} · ${build.gitRev}` : build.version}</dd>
        <dt>Channel</dt>
        <dd>{build.channel}</dd>
        <dt>Newest published</dt>
        <dd className={behind ? "behind" : undefined}>{build.latestVersion ?? "not read yet"}</dd>
        {checked && (
          <>
            <dt>Last checked</dt>
            <dd>{checked}</dd>
          </>
        )}
      </dl>
      {behind ? <UpdateInstructions /> : <p className="update-after">{releaseStateNote(build)}</p>}
    </div>
  );
}
