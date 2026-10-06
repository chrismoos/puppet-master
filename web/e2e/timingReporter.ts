import type { FullResult, Reporter, TestCase, TestResult } from "@playwright/test/reporter";

export default class TimingReporter implements Reporter {
  private results: Array<{ duration: number; retry: number; status: string; title: string }> = [];

  onTestEnd(test: TestCase, result: TestResult): void {
    this.results.push({
      duration: result.duration,
      retry: result.retry,
      status: result.status,
      title: test.titlePath().slice(1).join(" › "),
    });
  }

  onEnd(result: FullResult): void {
    const slowest = [...this.results].sort((left, right) => right.duration - left.duration).slice(0, 10);
    process.stdout.write("\n[e2e-timing] slowest tests:\n");
    for (const entry of slowest) {
      const retry = entry.retry > 0 ? ` retry=${entry.retry}` : "";
      process.stdout.write(`  ${(entry.duration / 1_000).toFixed(2)}s ${entry.status}${retry} ${entry.title}\n`);
    }
    const retries = this.results.filter((entry) => entry.retry > 0).length;
    process.stdout.write(`[e2e-timing] result=${result.status} attempts=${this.results.length} retries=${retries}\n`);
  }
}
