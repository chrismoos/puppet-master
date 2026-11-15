# Puppet Master

[![CI](https://github.com/chrismoos/puppet-master/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/chrismoos/puppet-master/actions/workflows/ci.yml)
[![Browser tests](https://github.com/chrismoos/puppet-master/actions/workflows/browser-lanes.yml/badge.svg?branch=master)](https://github.com/chrismoos/puppet-master/actions/workflows/browser-lanes.yml)

**All your agents in one place. At your desk and on your phone.**

Puppet Master runs and supervises real **Claude Code, Codex, Gemini, OpenCode,
and Antigravity** sessions across your projects. See what each agent is doing,
know when it needs you, and manage your work from the web dashboard. Pick up
the same sessions on your iPhone when you step away from your desk.

It runs on your own machine, with optional workers on other hosts or in
containers. Your agents use their existing CLIs, accounts, and model settings.

[Website](https://puppet-master.xyz/) · [Documentation](https://puppet-master.xyz/docs/) · [Get started](#get-started)

![Puppet Master dashboard showing the session fleet](https://puppet-master.xyz/media/shots/dashboard-fleet.webp)

## Your sessions, together

- **See the whole fleet.** Group projects into buckets, follow each session's
  activity, and find the agents waiting for a question or approval.
- **Keep sessions within reach.** The web dashboard and iOS app connect to the
  same live agent terminal and its scrollback. Close a browser tab without
  stopping the agent.
- **Give agents shared context.** Set bucket and project instructions, track
  tasks on the board, and work through plans with questions and decisions.
- **Review the work.** Ask an agent to open a code or document review. Leave
  comments, have it make changes, and follow the updated diff in your browser.
- **Share what they build.** Publish a running preview or a directory of files
  from a session and open it from another device.
- **Watch it work beside the agent.** Agents can open panes next to their
  terminal: a program running live, a dashboard page, or the app they are
  building. Resize, collapse, hide or stop them. The agent is told when you
  stop a pane it opened or start typing in one.
- **Hear when it matters.** Alerts stack as cards in the dashboard while you
  watch it, and reach your desktop or phone when you are away. Ask an agent to
  let you know when something finishes and it notifies you the same way.
- **Choose your models.** Configure model profiles and project defaults for
  supported agents, including custom provider endpoints.

## Get started

Prebuilt binaries are available for **macOS on Apple Silicon** and **Linux on
x86_64 and arm64**.

```sh
curl -fsSL https://dl.puppet-master.xyz/install.sh | sh
pm daemon
```

The installer puts `pm` in `/usr/local/bin` when writable, or `~/.local/bin`,
and updates your shell's `PATH` if needed.

Keep the daemon running and open **http://127.0.0.1:7676/**. Create your login,
add a bucket and a project directory, then start a session with your chosen
agent and a task. Browser terminals use Canvas 2D and WebAssembly.

By default, the local worker runs on the same machine as the daemon, so you
can start running agents without setting up another worker.

Install and authenticate the agent CLI you want to use. Puppet Master does
not include model access. See the [installation guide](https://puppet-master.xyz/docs/install/)
and [first session walkthrough](https://puppet-master.xyz/docs/first-session/).

## Run agents on workers

Start with the controller's local worker. Add workers when you want isolation
or sandboxing, a different environment, or more capacity. Run them on separate
machines or in containers, and choose the worker when you start a session.

Open **Settings → Workers** to configure a worker and generate its setup
command. Run the generated command on the machine that will host the worker.

Under **Bucket Access**, optionally select buckets to let all their projects
use the worker. Leave this empty to configure access to specific buckets or
projects afterward. Existing default workers stay the same.

Workers can run directly on a host or in an Incus, Docker, or Podman container.
For container workers, configure shared project directories and CPU and memory
limits in the worker setup.

See [worker setup](https://puppet-master.xyz/docs/guides/second-machine/)
for enrollment and remote access. Use HTTPS when accessing your controller
from another machine.

## Take your sessions with you on iOS

The companion iOS app connects to your controller so you can follow the fleet,
answer questions, approve calls, and use the same live agent terminal from
your phone. Return to the web dashboard to continue working at your desk.

See the [iOS guide](https://puppet-master.xyz/docs/guides/ios/) for setup and
[apps/mobile](apps/mobile/) for the app source.

## Use it as a Mac app

The desktop app opens your controller in its own window, with desktop
notifications and a Dock badge for sessions waiting on you. It is a client of
the controller you already run. See [apps/desktop](apps/desktop/) to build it.

A terminal has one shared size across viewers. Web and mobile let you choose
whether to resize it to fit your current view.

Drop a file on a session's terminal in the browser and it is stored on the
host that runs that session, with its path typed into the composer for you to
send. A session on a remote host gets the file on that host, where its agent
can read it.

## Let agents browse and show their work

Install the managed browser runtime once on each host that will browse:

```sh
pm browser install
```

This requires Node.js 22 or newer and npm. It installs pinned Playwright MCP
packages and Chromium for the current user. Minimal Linux hosts also need
Chromium's system libraries. Browser tools remain discoverable without the
runtime, but calls explain which host needs installation. Workers from older
builds must be updated before they can manage browsers. Existing sessions and
pane layouts remain compatible. Reconnect or resume an agent to refresh its
tool listing after an upgrade.

Ask an agent, for example, “Open the app, test signing in, and show me the
browser.” It uses the normal PM browser tools and `show_browser` opens a live,
read-only Browser pane in the web dashboard. There is no separate Playwright
MCP server to configure in the agent. Page-level JavaScript is available for
inspection and testing. Executing JavaScript inside the MCP server process is
not exposed.

Each session starts Chromium on its first browser operation and reuses one
default context. An agent can explicitly create a named context with
`browser_contexts` and a reason when a different login or separate cookies are
needed. Up to four contexts share that session's browser process. Cookies and
storage are isolated between contexts and between sessions.

Browser state stays available between agent turns, expires after fifteen
minutes without browser tool activity, and is cleared when the session ends.
Viewing does not keep it alive. Logins are temporary and do not survive a
browser or worker restart. An interrupted controller connection leaves the
worker's browser available until the idle timeout or session exit.

The pane follows the active tab in its selected context. Hiding, collapsing,
leaving the pane, or putting the dashboard tab in the background stops that
viewer's stream without stopping automation.
Closing a pane removes the viewer. `browser_contexts` closes browser contexts.
The browser viewport defaults to 1280 × 800, independent of pane size. The
pane scales the image to fit without changing the page's layout. Agents use
`browser_resize` when testing another viewport, from 320 × 240 through
1920 × 1200. If a browser expires, ask the
agent to show it again to open a fresh one.

The runtime is stored under the user's data directory. Set
`PM_BROWSER_RUNTIME_DIR` consistently for installation and the worker or daemon
to use a different directory. `PM_BROWSER_IDLE_MS` can override the idle timeout
in milliseconds for development. Browser runtime setup is optional for hosts
that do not use managed browsing.

## Connect tools with approvals

Add an HTTP MCP server or import a REST API's OpenAPI document in
**Settings → Connections**. Agents can draft connection setup and policy
changes for you to review. Enter credentials in the dashboard, then choose
which tools may run and which need approval.

The controller makes downstream requests and keeps credentials out of the
agent conversation. Held calls show their arguments before you approve them,
and call history records the result. Workers access their project's
connections; supervisors can access connections across their bucket. New and
unclassified tools require approval.

Read the [connections guide](https://puppet-master.xyz/docs/guides/connections/)
for setup, OAuth, and permission policies.

## Updates and security

Puppet Master is early software. The CLI, database schema, and worker protocol
can change between releases. Read the [changelog](CHANGELOG.md) before updating.

```sh
pm update
```

Use `pm update --channel dev` to follow pre-releases, or
`pm update --channel stable` to return to stable releases. Workers follow their
controller's build.

The daemon checks its channel every six hours and reports what it finds. When a
newer build is published, the version in the dashboard's topbar carries an
update mark and opens the command above. Settings → About shows the build this
controller runs, the channel it follows, and when it last looked.

The dashboard binds to localhost by default and is intended for a single
trusted user. Access gives control of agents and terminals on your machine.
Read [SECURITY.md](SECURITY.md) before enabling remote access or sharing previews.

## Learn more

- [Documentation](https://puppet-master.xyz/docs/) — installation, everyday use, and configuration.
- [Code reviews](https://puppet-master.xyz/docs/guides/reviews/) — review diffs and documents with your agents.
- [Contributing](CONTRIBUTING.md) — build from source and run the test suites.
- [Changelog](CHANGELOG.md) — release changes and compatibility notes.

## License

MIT. See [LICENSE](LICENSE) and [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
