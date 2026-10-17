import { describe, expect, it } from "vitest";

import { spokenVocabulary } from "./vocabulary";

describe("what the recognizer is told to expect", () => {
  it("picks out the name that gets misheard", () => {
    const said = spokenVocabulary([{ goal: "Optimizing iDEN femtocell software" }]);
    expect(said).toContain("iDEN");
    expect(said).toContain("Optimizing iDEN femtocell software");
    // Ordinary English is what recognition already knows.
    expect(said).not.toContain("software");
  });

  it("counts a model number and an acronym as worth expecting", () => {
    const said = spokenVocabulary([
      { goal: "Phone 1000 handover" },
      { headline: "Checking the ASR path" },
    ]);
    expect(said).toContain("1000");
    expect(said).toContain("ASR");
  });

  it("takes the unusual words out of a sentence but not the sentence", () => {
    const headline = "Chasing playback clicks and adding a route toggle to the iDEN build";
    const said = spokenVocabulary([{ headline }]);
    expect(said).toContain("iDEN");
    expect(said).not.toContain(headline);
  });

  it("keeps the first spelling and says each thing once", () => {
    const said = spokenVocabulary([
      { goal: "iDEN work" },
      { goal: "IDEN work" },
      { name: "iden" },
    ]);
    expect(said.filter((word) => word.toLowerCase() === "iden")).toEqual(["iDEN"]);
    expect(said.filter((phrase) => phrase.toLowerCase() === "iden work")).toEqual(["iDEN work"]);
  });

  it("stays short enough that each phrase still carries weight", () => {
    const many = Array.from({ length: 200 }, (_, n) => ({ goal: `Build${n} rollout` }));
    expect(spokenVocabulary(many)).toHaveLength(60);
  });

  it("says nothing when there is nothing named", () => {
    expect(spokenVocabulary([])).toEqual([]);
    expect(spokenVocabulary([{ goal: "   " }, {}])).toEqual([]);
  });
});
