# Changelog

## Unreleased

- New `spawn.program_status` setting, off by default: agent terminals answer the Program Status Protocol (OSC 7501), and an agent that reports through it drives its session state ahead of lifecycle hooks, with its message and progress in the state detail.
- Interrupting a Codex turn immediately marks the session idle with an interruption reason.

- Switching terminals waits for the destination to render at the pane size before showing it.
- Browser tests select the installed test agent and verify scrollbar timers independently of runner speed.
- Killing a session in the iOS app returns to the session list, where a session that just ended stays listed and can be resumed.
- The iOS session menu no longer offers Interrupt.

## 0.10.0 — 2026-10-06

- Initial release.
