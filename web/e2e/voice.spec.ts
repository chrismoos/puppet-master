import { createServer, type Server } from "node:http";
import { AddressInfo } from "node:net";

import { expect, test } from "./fixtures";
import { apiHeaders, logIn } from "./support";

/**
 * The voice panel against a scripted model, so the browser path is
 * exercised without a provider account. No speech provider is
 * configured, which is the state the answer still has to arrive in.
 */

const PROFILE = "voice-e2e";

/**
 * A speech endpoint returning real PCM16 bytes, so the browser's audio path
 * is exercised without a provider account. The samples are a quiet ramp
 * rather than silence, so a sink that drops them is distinguishable from one
 * that plays them.
 */
async function scriptedSpeech(): Promise<{ base: string; close: () => Promise<void> }> {
  const frames = 2;
  const samplesPerFrame = 1200;
  const server: Server = createServer((request, response) => {
    request.on("data", () => {});
    request.on("end", () => {
      response.writeHead(200, { "content-type": "application/octet-stream" });
      for (let f = 0; f < frames; f += 1) {
        const buffer = Buffer.alloc(samplesPerFrame * 2);
        for (let i = 0; i < samplesPerFrame; i += 1) {
          buffer.writeInt16LE(((i % 256) - 128) * 64, i * 2);
        }
        response.write(buffer);
      }
      response.end();
    });
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address() as AddressInfo;
  return {
    base: `http://127.0.0.1:${port}`,
    close: () =>
      new Promise<void>((resolve) => {
        server.close(() => resolve());
      }),
  };
}

/** Answers every request with one scripted reply. */
async function scriptedModel(reply: string): Promise<{ base: string; close: () => Promise<void> }> {
  const server: Server = createServer((request, response) => {
    let body = "";
    request.on("data", (chunk) => {
      body += chunk;
    });
    request.on("end", () => {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(
        JSON.stringify({
          content: [{ type: "text", text: reply }],
          stop_reason: "end_turn",
        }),
      );
    });
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address() as AddressInfo;
  return {
    base: `http://127.0.0.1:${port}`,
    close: () =>
      new Promise<void>((resolve) => {
        server.close(() => resolve());
      }),
  };
}

test("the voice panel answers a question typed into it", async ({
  page,
  isolatedDaemon: _isolatedDaemon,
}) => {
  const model = await scriptedModel("Nothing needs you right now.");
  try {
    await logIn(page);
    const headers = await apiHeaders(page);
    const base = process.env.PM_E2E_BASE_URL!;

    // A profile carries the endpoint and the credential, so voice shares
    // the configuration every other model use already has.
    await page.goto(`${base}#/settings/models`);
    const panel = page.locator(".model-profiles");
    await panel.getByLabel("Name", { exact: true }).fill(PROFILE);
    await panel.getByLabel("API key", { exact: true }).fill("placeholder-key");
    await panel.getByRole("button", { name: "Create profile" }).click();
    const profile = page.locator(".model-profile").first();
    await expect(profile).toBeVisible();
    await profile.locator(".catalog-name").click();
    const anthropic = profile.locator('.model-endpoint[data-dialect="anthropic-messages"]');
    await anthropic.getByLabel("model", { exact: true }).fill("scripted-model");
    await anthropic.getByLabel("base URL").fill(model.base);
    await anthropic.getByRole("button", { name: "Add endpoint" }).click();
    await expect(profile).toContainText("covers Claude");

    const set = await page.request.put(`${base}/api/settings/voice.model.profile`, {
      headers,
      data: { value: PROFILE },
    });
    expect(set.ok()).toBeTruthy();

    await page.goto(base);
    await page.getByRole("button", { name: "talk to Puppet Master" }).click();
    const voice = page.getByRole("complementary", { name: "Voice" });
    await expect(voice).toBeVisible();

    await voice.getByLabel("What to say").fill("what needs me");
    await voice.getByRole("button", { name: "Send", exact: true }).click();

    // What the user said appears at once, and the answer follows over the
    // same socket the dashboard already holds.
    await expect(voice).toContainText("what needs me");
    await expect(voice).toContainText("Nothing needs you right now.");
  } finally {
    await model.close();
  }
});

test("the panel says why a turn failed when nothing is configured", async ({
  page,
  isolatedDaemon: _isolatedDaemon,
}) => {
  await logIn(page);
  const base = process.env.PM_E2E_BASE_URL!;
  await page.goto(base);
  await page.getByRole("button", { name: "talk to Puppet Master" }).click();
  const voice = page.getByRole("complementary", { name: "Voice" });
  await voice.getByLabel("What to say").fill("what needs me");
  await voice.getByRole("button", { name: "Send", exact: true }).click();

  // An operator setting no retry fixes, so the panel names it rather than
  // leaving the user waiting on an answer that is never coming.
  await expect(voice).toContainText("voice.model.profile");
});

/**
 * The browser's own audio path, end to end: a scripted provider's PCM bytes
 * travel daemon to socket to page, and the page decodes them into buffers
 * and schedules them for playback.
 *
 * AudioContext is wrapped in the page before anything loads, so the count of
 * decoded buffers is observed rather than inferred. Playwright cannot hear
 * sound, and asserting the absence of an error would pass just as well
 * against a sink that silently dropped every frame.
 */
test("rendered speech reaches the page and is scheduled for playback", async ({
  page,
  isolatedDaemon: _isolatedDaemon,
}) => {
  const model = await scriptedModel("All quiet.");
  const speech = await scriptedSpeech();
  try {
    await page.addInitScript(() => {
      const w = window as unknown as {
        AudioContext: typeof AudioContext;
        __pmAudio: { buffers: number; frames: number; started: number };
      };
      w.__pmAudio = { buffers: 0, frames: 0, started: 0 };
      const Real = w.AudioContext;
      w.AudioContext = class extends Real {
        createBuffer(channels: number, length: number, rate: number) {
          w.__pmAudio.buffers += 1;
          w.__pmAudio.frames += length;
          return super.createBuffer(channels, length, rate);
        }
        createBufferSource() {
          const source = super.createBufferSource();
          const start = source.start.bind(source);
          source.start = (...args: Parameters<AudioBufferSourceNode["start"]>) => {
            w.__pmAudio.started += 1;
            return start(...args);
          };
          return source;
        }
      } as unknown as typeof AudioContext;
    });

    await logIn(page);
    const headers = await apiHeaders(page);
    const base = process.env.PM_E2E_BASE_URL!;

    await page.goto(`${base}#/settings/models`);
    const panel = page.locator(".model-profiles");
    await panel.getByLabel("Name", { exact: true }).fill(PROFILE);
    await panel.getByLabel("API key", { exact: true }).fill("placeholder-key");
    await panel.getByRole("button", { name: "Create profile" }).click();
    const profile = page.locator(".model-profile").first();
    await expect(profile).toBeVisible();
    await profile.locator(".catalog-name").click();
    const anthropic = profile.locator('.model-endpoint[data-dialect="anthropic-messages"]');
    await anthropic.getByLabel("model", { exact: true }).fill("scripted-model");
    await anthropic.getByLabel("base URL").fill(model.base);
    await anthropic.getByRole("button", { name: "Add endpoint" }).click();
    await expect(profile).toContainText("covers Claude");

    for (const [key, value] of [
      ["voice.model.profile", PROFILE],
      ["voice.tts.base_url", speech.base],
      ["voice.tts.voice", "a-voice"],
      ["voice.tts.api_key", "placeholder-key"],
    ] as const) {
      const set = await page.request.put(`${base}/api/settings/${key}`, {
        headers,
        data: { value },
      });
      expect(set.ok(), `${key} was accepted`).toBeTruthy();
    }

    await page.goto(base);
    await page.getByRole("button", { name: "talk to Puppet Master" }).click();
    const voice = page.getByRole("complementary", { name: "Voice" });
    await expect(voice).toBeVisible();
    // Speaking is on by default, so the audio leg is exercised as shipped.
    await expect(voice.getByLabel("speak answers")).toBeChecked();

    await voice.getByLabel("What to say").fill("what needs me");
    await voice.getByRole("button", { name: "Send", exact: true }).click();
    await expect(voice).toContainText("All quiet.");

    await expect
      .poll(
        () => page.evaluate(() => (window as unknown as { __pmAudio: { buffers: number } }).__pmAudio.buffers),
        { message: "the page decoded the provider's frames into audio buffers" },
      )
      .toBeGreaterThan(0);
    const audio = await page.evaluate(
      () => (window as unknown as { __pmAudio: { buffers: number; frames: number; started: number } }).__pmAudio,
    );
    expect(audio.started, "every buffer was scheduled to play").toBeGreaterThan(0);
    // 1200 samples per frame from the scripted provider, so a partial or
    // dropped frame reads as a smaller total rather than passing silently.
    expect(audio.frames, "whole frames arrived").toBeGreaterThanOrEqual(1200);
    await expect(voice.locator(".voice-trouble")).toHaveCount(0);
  } finally {
    await speech.close();
    await model.close();
  }
});
