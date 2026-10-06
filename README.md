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
agent and a task. Browser terminals require WebGL2.

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

A terminal has one shared size across viewers. Web and mobile let you choose
whether to resize it to fit your current view.

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
