## What this changes

<!-- What behavior is different, and why. If it fixes an issue, link it. -->

## How it was verified

<!--
Which of these you ran, and on what. `make check` and `make test` are expected
on every change. A change that touches xterm, layout, focus, routing, resizing
or selection needs a real browser lane too.
-->

- [ ] `make check`
- [ ] `make test`
- [ ] `make e2e-integration`
- [ ] A focused browser lane (`make e2e-worker E2E_SPECS=...`) — which:
- [ ] Verified on a device or simulator, for a change under `apps/mobile`

Say plainly what you could not verify and why.

## Checklist

- [ ] Tests cover the change, at the layer that owns the behavior.
- [ ] User-facing features, CLI flags and configuration are documented in this
      same change.
- [ ] Commit messages follow `CONTRIBUTING.md`: imperative subject ending in a
      period, no semicolons, and no attribution or provenance trailers.
- [ ] No external URLs and no secrets added to the tree.
- [ ] The version in `Cargo.toml` is untouched.
