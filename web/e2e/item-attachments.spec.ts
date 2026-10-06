import { expect, test } from "./fixtures";
import { logIn } from "./support";

test("Board uploads, lists, downloads, and confirms attachment deletion", async ({ page }) => {
  await logIn(page);
  await page.locator(".sb-board-link").click();
  await expect(page.locator(".workbench-inspector")).toBeVisible();

  const attachments = page.locator(".attachment-section");
  await expect(attachments.getByText("No attachments yet.")).toBeVisible();
  const input = attachments.locator('input[type="file"]');
  await input.setInputFiles([
    { name: "résumé 版本.txt", mimeType: "text/plain", buffer: Buffer.from("hello attachment") },
    { name: "empty.bin", mimeType: "", buffer: Buffer.alloc(0) },
  ]);

  await expect(attachments.getByRole("link", { name: "résumé 版本.txt" })).toBeVisible();
  await expect(attachments.getByRole("link", { name: "empty.bin" })).toBeVisible();
  await expect(attachments).toContainText("text/plain · 16 B · you");
  await expect(attachments).toContainText("application/octet-stream · 0 B · you");
  await expect(page.locator(".activity-section")).toContainText("attached résumé 版本.txt");

  const downloadPromise = page.waitForEvent("download");
  await attachments.getByRole("link", { name: "résumé 版本.txt" }).click();
  const download = await downloadPromise;
  expect(download.suggestedFilename()).toBe("résumé 版本.txt");
  const stream = await download.createReadStream();
  const chunks: Buffer[] = [];
  for await (const chunk of stream) chunks.push(Buffer.from(chunk));
  expect(Buffer.concat(chunks).toString()).toBe("hello attachment");

  page.once("dialog", (dialog) => dialog.dismiss());
  await attachments.getByRole("button", { name: "remove résumé 版本.txt" }).click();
  await expect(attachments.getByRole("link", { name: "résumé 版本.txt" })).toBeVisible();

  page.once("dialog", (dialog) => dialog.accept());
  await attachments.getByRole("button", { name: "remove résumé 版本.txt" }).click();
  await expect(attachments.getByRole("link", { name: "résumé 版本.txt" })).toHaveCount(0);
  await expect(page.locator(".activity-section")).toContainText("removed attachment résumé 版本.txt");
});
