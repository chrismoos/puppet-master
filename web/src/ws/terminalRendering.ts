export interface ResumableTerminal {
  refresh(start: number, end: number): void;
  rows: number;
}

export function resumeRendering(term: ResumableTerminal): void {
  term.refresh(0, Math.max(0, term.rows - 1));
}
