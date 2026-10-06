import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { LOCAL_WORKER_ID } from "@puppet-master/client-core/format";
import { UnavailableWorkerChip } from "./WorkerChip";

describe("unavailable worker chip", () => {
  it("identifies a disabled local worker", () => {
    const markup = renderToStaticMarkup(createElement(UnavailableWorkerChip, { workerId: LOCAL_WORKER_ID }));
    expect(markup).toContain("local");
    expect(markup).toContain("disabled");
    expect(markup).toContain("Local worker is disabled for this daemon");
  });
});
