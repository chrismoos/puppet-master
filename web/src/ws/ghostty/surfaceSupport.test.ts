import { describe, expect, it } from "vitest";
import { extractTerminalLinks } from "./surfaceSupport";

describe("terminal URL boundaries", () => {
  const url = "https://example.test/path";

  it.each([
    [`(${url})`, url],
    [`((${url}))`, url],
    [`[details](${url})`, url],
    [`(${url}).`, url],
    [`(${url}.)`, url],
    [`(${url}), next`, url],
    [`(${url}?!)).`, url],
    [`(${url}_(details))`, `${url}_(details)`],
    [`(${url}_(nested_(details)))`, `${url}_(nested_(details))`],
    [`${url}_(details)`, `${url}_(details)`],
    [`${url}?q=(details)`, `${url}?q=(details)`],
    [`${url}%28details%29`, `${url}%28details%29`],
    [`${url}(details`, `${url}(details`],
    ["(pm:session/42)", "pm:session/42"],
  ])("extracts the destination and exact range from %s", (text, destination) => {
    const start = text.indexOf(destination);
    expect(extractTerminalLinks(text)).toEqual([
      { text: destination, start, end: start + destination.length },
    ]);
  });

  it("keeps the offsets of later links after trimming an earlier one", () => {
    const text = `See (${url}) and ${url}_(details).`;
    const first = text.indexOf(url);
    const second = text.lastIndexOf(url);
    const destination = `${url}_(details)`;
    expect(extractTerminalLinks(text)).toEqual([
      { text: url, start: first, end: first + url.length },
      { text: destination, start: second, end: second + destination.length },
    ]);
  });
});
