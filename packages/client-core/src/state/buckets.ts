import { sessionEnded } from "../format";
import { SessionState } from "../gen/pm/v1/pm_pb";
import { awaitsUser } from "./displayState";
import type { AppState } from "./reducer";

const MONOGRAM_LETTERS = 2;
const LONG_FACE_CHARS = 3;

function words(name: string): string[] {
  return name.split(/[\s._-]+/).filter(Boolean);
}

/** Two letters for a rail button: the first two words' initials, or the first two letters of one word. */
export function bucketMonogram(name: string): string {
  const parts = words(name);
  const letters = parts.length >= MONOGRAM_LETTERS
    ? parts.slice(0, MONOGRAM_LETTERS).map((word) => [...word][0]).join("")
    : [...(parts[0] ?? "?")].slice(0, MONOGRAM_LETTERS).join("");
  return letters.toUpperCase();
}

/** Letters to try when the plain monogram is taken: the first letter with each later one, then three letters. */
function monogramCandidates(name: string): string[] {
  const letters = [...words(name).join("")].filter((char) => /\p{L}|\p{N}/u.test(char));
  const first = letters[0] ?? "?";
  const pairs = letters.slice(1).map((letter) => `${first}${letter}`);
  const parts = words(name);
  const triple = parts.length >= LONG_FACE_CHARS
    ? parts.slice(0, LONG_FACE_CHARS).map((word) => [...word][0]).join("")
    : letters.slice(0, LONG_FACE_CHARS).join("");
  return [bucketMonogram(name), ...pairs, triple].map((candidate) => candidate.toUpperCase());
}

/** Letters for every bucket, distinct wherever the names allow, earlier buckets keeping their plain monogram. */
export function railMonograms(buckets: readonly { id: string; name: string }[]): Map<string, string> {
  const plain = new Map<string, number>();
  for (const bucket of buckets) {
    const monogram = bucketMonogram(bucket.name);
    plain.set(monogram, (plain.get(monogram) ?? 0) + 1);
  }
  const taken = new Set<string>();
  const result = new Map<string, string>();
  for (const bucket of buckets) {
    const monogram = bucketMonogram(bucket.name);
    const candidates = plain.get(monogram) === 1 ? [monogram] : monogramCandidates(bucket.name);
    const pick = candidates.find((candidate) => !taken.has(candidate)) ?? monogram;
    taken.add(pick);
    result.set(bucket.id, pick);
  }
  return result;
}

export interface BucketTally {
  live: number;
  needsInput: number;
  needsInputUnseen: number;
  /** Idle sessions whose finished turn has not been viewed. */
  finishedUnseen: number;
  /** Sessions whose agent is mid-turn. */
  working: number;
}

/** Live and needs-you counts per bucket id, over every session that has not ended. */
export function bucketTallies(state: Pick<AppState, "projects" | "sessions">): Map<string, BucketTally> {
  const bucketOfProject = new Map<string, string>();
  for (const project of state.projects.values()) {
    bucketOfProject.set(project.id.toString(), project.bucketId.toString());
  }
  const tallies = new Map<string, BucketTally>();
  for (const session of state.sessions.values()) {
    if (sessionEnded(session)) continue;
    const bucketId = bucketOfProject.get(session.projectId.toString());
    if (bucketId === undefined) continue;
    const tally = tallies.get(bucketId) ?? { live: 0, needsInput: 0, needsInputUnseen: 0, finishedUnseen: 0, working: 0 };
    tally.live += 1;
    if (awaitsUser(session, (id) => state.sessions.get(id))) {
      tally.needsInput += 1;
      if (session.needsInputUnseen) tally.needsInputUnseen += 1;
    }
    if (session.state === SessionState.IDLE && session.idleUnseen) tally.finishedUnseen += 1;
    if (session.state === SessionState.WORKING) tally.working += 1;
    tallies.set(bucketId, tally);
  }
  return tallies;
}

