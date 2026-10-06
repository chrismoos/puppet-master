// @vitest-environment jsdom
import { create } from "@bufbuild/protobuf";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { BucketSchema } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import * as api from "../api/workers";
import { AddWorkerForm } from "./WorkersPanel";

vi.mock("../api/workers", async (importOriginal) => ({
  ...await importOriginal<typeof import("../api/workers")>(),
  enrollWorker: vi.fn(),
}));

let host: HTMLDivElement;
let root: Root;

beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.mocked(api.enrollWorker).mockReset().mockResolvedValue({ token: "test-token", expiresAtUnixMs: 5000 });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  await act(async () => root.render(
    <AddWorkerForm
      build={null}
      workers={[]}
      buckets={[create(BucketSchema, { id: 3n, name: "Work" }), create(BucketSchema, { id: 8n, name: "Personal" })]}
      copied={null}
      onCopy={() => {}}
      onClose={() => {}}
    />,
  ));
  const name = host.querySelector<HTMLInputElement>("#add-worker-name")!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(name, "build-box");
    name.dispatchEvent(new Event("input", { bubbles: true }));
  });
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});

async function generate() {
  await act(async () => host.querySelector<HTMLButtonElement>('button[type="submit"]')!.click());
}

it("enrolls with multiple selected buckets and preserves the command after granting access", async () => {
  const choices = host.querySelectorAll<HTMLInputElement>('input[type="checkbox"]');
  for (const choice of choices) await act(async () => choice.click());
  expect(host.querySelector("summary")!.textContent).toBe("2 selected");
  await generate();
  expect(api.enrollWorker).toHaveBeenCalledExactlyOnceWith("build-box", undefined, [3n, 8n]);
  expect(host.textContent).toContain("Run this on the worker");
  expect(host.querySelector<HTMLFieldSetElement>("fieldset")!.disabled).toBe(true);
});

it("allows setup without granting any bucket access", async () => {
  await generate();
  expect(api.enrollWorker).toHaveBeenCalledExactlyOnceWith("build-box", undefined, []);
});

it("removes a deselected bucket from the grant", async () => {
  const choices = host.querySelectorAll<HTMLInputElement>('input[type="checkbox"]');
  await act(async () => choices[0].click());
  await act(async () => choices[1].click());
  await act(async () => choices[0].click());
  await generate();
  expect(api.enrollWorker).toHaveBeenCalledExactlyOnceWith("build-box", undefined, [8n]);
});

it("keeps selections editable when enrollment fails", async () => {
  vi.mocked(api.enrollWorker).mockRejectedValue(new Error("bucket no longer exists"));
  await act(async () => host.querySelector<HTMLInputElement>('input[type="checkbox"]')!.click());
  await generate();
  expect(host.querySelector('[role="alert"]')!.textContent).toBe("bucket no longer exists");
  expect(host.querySelector<HTMLFieldSetElement>("fieldset")!.disabled).toBe(false);
  expect(host.querySelector<HTMLInputElement>('input[type="checkbox"]')!.checked).toBe(true);
  expect(host.textContent).not.toContain("Run this on the worker");
});
