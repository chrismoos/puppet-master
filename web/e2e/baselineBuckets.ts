type Cli = (args: string[]) => string;

function bucketRows(output: string): Array<{ id: number; name: string }> {
  return output
    .trim()
    .split("\n")
    .slice(1)
    .filter(Boolean)
    .map((line) => {
      const match = line.match(/^\s*(\d+)\s+(.+?)\s*$/);
      if (!match) throw new Error(`unexpected bucket listing row: ${line}`);
      return { id: Number(match[1]), name: match[2] };
    });
}

const SEEDED_PROJECT_ID = "1";

/** Replace the production first-run seed with the browser suite's isolated baseline. */
export function establishBrowserBucket(cli: Cli): number {
  const initial = bucketRows(cli(["bucket", "ls"]));
  if (initial.length !== 1 || initial[0].id !== 1 || initial[0].name !== "Default") {
    throw new Error(`expected fresh Default bucket, got ${JSON.stringify(initial)}`);
  }

  cli(["project", "rm", SEEDED_PROJECT_ID]);
  cli(["bucket", "rm", "1"]);
  const created = cli(["bucket", "add", "browser-e2e"]);
  const match = created.match(/^bucket (\d+) created\s*$/);
  const bucketId = Number(match?.[1]);
  if (bucketId !== 1) {
    throw new Error(`expected browser-e2e bucket id 1, got ${created.trim()}`);
  }

  const final = bucketRows(cli(["bucket", "ls"]));
  if (final.length !== 1 || final[0].id !== bucketId || final[0].name !== "browser-e2e") {
    throw new Error(`expected isolated browser-e2e bucket, got ${JSON.stringify(final)}`);
  }
  return bucketId;
}
