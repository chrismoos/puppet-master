import { execFileSync } from "node:child_process";
import { expect, test } from "./fixtures";
import { logIn } from "./support";

function cli(args: string[]): string {
  return execFileSync(process.env.PM_E2E_PM_BIN!, args, {
    env: { ...process.env, PM_SOCKET: process.env.PM_E2E_SOCKET },
    encoding: "utf8",
  });
}

function createdItem(output: string, bucketId: string): string {
  const id = output.match(new RegExp(`pm:item/${bucketId}/(\\d+) created`))?.[1];
  if (!id) throw new Error(`could not parse item reference from: ${output}`);
  return id;
}

test("bucket-qualified item routes isolate equal numbers and copy canonical references", async ({ page, context }) => {
  const activeBoard = () => page.locator(".board-route-view.is-active");
  const firstBucket = cli(["bucket", "add", "identity-one"]).match(/bucket (\d+) created/)?.[1];
  const secondBucket = cli(["bucket", "add", "identity-two"]).match(/bucket (\d+) created/)?.[1];
  if (!firstBucket || !secondBucket) throw new Error("could not create identity test buckets");
  const firstId = createdItem(
    cli(["items", "add", "--bucket", firstBucket, "--status", "planned", "first bucket item"]),
    firstBucket,
  );
  const secondId = createdItem(
    cli(["items", "add", "--bucket", secondBucket, "--status", "planned", "second bucket item"]),
    secondBucket,
  );
  expect(firstId).toBe("1");
  expect(secondId).toBe("1");

  await logIn(page);
  await page.goto(`${process.env.PM_E2E_BASE_URL!}/#/bucket/${firstBucket}/item/1`);
  await expect(activeBoard().getByLabel("item title")).toHaveValue("first bucket item");
  await expect(activeBoard().getByRole("link", { name: `pm:item/${firstBucket}/1` })).toHaveAttribute(
    "href",
    `#/bucket/${firstBucket}/item/1`,
  );

  await context.grantPermissions(["clipboard-read", "clipboard-write"], {
    origin: process.env.PM_E2E_BASE_URL!,
  });
  await page.getByRole("button", { name: `Copy item reference pm:item/${firstBucket}/1` }).click();
  await expect(page.getByRole("status").filter({ hasText: "Copied" })).toHaveText("Copied");
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(`pm:item/${firstBucket}/1`);

  await page.goto(`${process.env.PM_E2E_BASE_URL!}/#/bucket/${secondBucket}/item/1`);
  await expect(activeBoard().getByLabel("item title")).toHaveValue("second bucket item");
  await expect(activeBoard().getByRole("link", { name: `pm:item/${secondBucket}/1` })).toBeVisible();

  // Public item 2 does not exist in bucket 1. The foreign row's old global
  // surrogate must not make this route resolve across the bucket boundary.
  await page.goto(`${process.env.PM_E2E_BASE_URL!}/#/bucket/${firstBucket}/item/2`);
  await expect(page.getByText(/item not found: 2/i)).toBeVisible();
  await expect(activeBoard().getByLabel("item title")).toHaveCount(0);

  await page.goto(`${process.env.PM_E2E_BASE_URL!}/#/item/1`);
  await expect(page.getByText(/Legacy item links are unsupported/i)).toBeVisible();
  await expect(activeBoard().getByLabel("item title")).toHaveCount(0);
});
