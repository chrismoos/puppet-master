import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";
import { postPlanMessage, submitPlanDecision } from "./plans";
import { seedAccessToken } from "./token.fixture";

// Every authenticated call mints a token when it holds none, which would
// otherwise be the first call a stub answers.
beforeEach(seedAccessToken);

afterEach(() => vi.unstubAllGlobals());

describe("plan API", () => {
  it("submits choices, a custom option, and notes together", async () => {
    const fetch = vi.fn().mockResolvedValue(new Response("{}", { status: 200 }));
    vi.stubGlobal("fetch", fetch);
    await submitPlanDecision("7", 9, {
      selectedOptionKeys: ["postgres"],
      customLabel: "Hybrid",
      customDetailMarkdown: "Use both.",
      notes: { postgres: "Preferred" },
    });
    expect(fetch).toHaveBeenCalledWith(
      "/api/plans/7/decisions/9/respond",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({
          selectedOptionKeys: ["postgres"],
          customLabel: "Hybrid",
          customDetailMarkdown: "Use both.",
          notes: { postgres: "Preferred" },
        }),
      }),
    );
  });

  it("keeps decision dialogue open while an answer is waiting", async () => {
    const fetch = vi.fn().mockResolvedValue(new Response("{}", { status: 200 }));
    vi.stubGlobal("fetch", fetch);
    await postPlanMessage("7", 9, "One more constraint.");
    expect(fetch).toHaveBeenCalledWith(
      "/api/plans/7/messages",
      expect.objectContaining({ body: JSON.stringify({ decisionId: 9, body: "One more constraint." }) }),
    );
  });
});
