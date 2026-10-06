# Security

Puppet Master runs on infrastructure you control and is designed for a single
trusted user. These are the main parts of its security architecture:

- **Authenticated dashboard and apps.** Web, iOS, and remote CLI access require
  sign-in. Passwords use Argon2id hashing, and dashboard access tokens stay in
  memory.
  The dashboard listens on localhost by default. Use HTTPS for remote access.
- **Private local access.** The local daemon socket, database, credentials,
  and terminal history use owner-only filesystem permissions.
- **Authenticated workers.** The controller and workers communicate over
  TLS 1.3 connections with mutual TLS (mTLS) authentication and pinned peer
  identities. New workers join through short-lived, single-use enrollment tokens.
- **Sealed credentials.** Stored provider and connection credentials use
  ChaCha20-Poly1305 authenticated encryption. The installation sealing secret
  is kept in a separate owner-only file beside the database. Stored credentials
  are write-only through the API. The controller makes connection requests,
  so agents receive tool results without receiving connection credentials.
- **OAuth protection.** Connection sign-in uses PKCE with SHA-256 (S256) and
  short-lived, single-use callback state.
- **Scoped tools and approvals.** Workers access their project's connections;
  supervisors access connections within their bucket. You choose which tool
  calls need approval, review their arguments, and inspect their call history.
- **Optional worker isolation.** Container workers provide a separate
  environment with selected shared directories and resource limits. Agents
  running directly on a host have the permissions of the user running them.
- **Controlled sharing.** HTTP previews and shared directories require sign-in
  or a scoped access token. Use a share domain to separate previews from the
  dashboard's browser origin. Raw TCP forwards use the forwarded service's
  own access controls.
- **Verified updates.** Installed binaries verify Ed25519 release signatures
  with a built-in public key and check SHA-256 content digests. The initial
  installer downloads over HTTPS and checks the published digest.

## Managing access

- Manage enrolled workers and mobile devices in Settings. Remove a worker or
  revoke a device to end its access.
- Security notices highlight device enrollment, worker enrollment or
  re-enrollment, and changes to shared instructions. The related settings and
  instruction history let you review those changes.
- Terminal history contains whatever your agents print. Treat it as part of
  your project data when storing backups or sharing output.

## Reporting a vulnerability

Use a private security advisory on this repository. Include the version or
commit, what you observed, and steps to reproduce it.

Reports about the daemon, web and iOS apps, worker connections, and agent
integrations are welcome. Issues in the coding agents themselves should go to
their vendors.
