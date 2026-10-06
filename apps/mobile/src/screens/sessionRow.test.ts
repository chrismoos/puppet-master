import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { SessionSchema } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { sessionRowSubtitle } from "./sessionRow";

describe("sessionRowSubtitle", () => {
  const session = create(SessionSchema, {
    id: 3n,
    goal: "Optimizing femtocell software",
    headline: "Editing femtocell.rs 2/3",
  });

  it("shows the headline under the goal", () => {
    expect(sessionRowSubtitle(session, "local", false)).toBe("Editing femtocell.rs 2/3");
  });

  it("leads with the host while searching", () => {
    expect(sessionRowSubtitle(session, "hnb", true)).toBe("hnb · Editing femtocell.rs 2/3");
  });

  it("is empty when the title already is the headline", () => {
    const unnamed = create(SessionSchema, { id: 3n, headline: "Editing femtocell.rs" });
    expect(sessionRowSubtitle(unnamed, "", false)).toBe("");
  });
});
