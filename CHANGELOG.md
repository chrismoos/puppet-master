# Changelog

## Unreleased

- New `spawn.program_status` setting, off by default: agent terminals answer the Program Status Protocol (OSC 7501), and an agent that reports through it drives its session state ahead of lifecycle hooks, with its message and progress in the state detail.
- Interrupting a Codex turn immediately marks the session idle with an interruption reason.
- A session whose agent is killed on its host is resumed instead of left ended, up to three times, and says which signal ended it.
- A session ended by a signal reports 128 plus the signal, so a killed agent reads 137 rather than 1.

- Switching terminals waits for the destination to render at the pane size before showing it.
- Browser tests select the installed test agent and verify scrollbar timers independently of runner speed.
- [iOS] Tapping a link in terminal output opens it, the key row keeps its keys in reach, and a sticky Alt modifier joins Shift and Ctrl.
- [iOS] The spawn sheet offers every agent and opens the session it creates, and clearing filters closes the filter bar.
- [iOS] A tapped forward link opens signed in for that forward, at any path under it, instead of on the sign-in page.
- Killing a session in the iOS app returns to the session list, where a session that just ended stays listed and can be resumed.
- The iOS session menu no longer offers Interrupt.

## 0.10.0 — 2026-10-06

- Initial release.
