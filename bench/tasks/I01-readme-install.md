---
id: I01
kind: implementation
role: implementation
title: Stop the README telling users to install an unpublished package
mini: true
scope: [README.md, CHANGELOG.md]
validator: validate/I01.mjs
---

## Brief
`README.md` opens its Quick start with `npm install -g agent-communication-system` (and an `npx -p agent-communication-system` alternative). That package is not on the npm registry: `npm view agent-communication-system` is a 404, and version 0.2.0 exists only as a GitHub tag. A first-time reader who follows the first instruction gets an error.

Change the README so the Quick start leads with the path that works today (clone, `npm ci`, `npm run build`, `npm link`), and so the npm commands are either removed or clearly marked as available only after the first npm release. Also correct the `CHANGELOG.md` heading text that calls 0.2.0 a "first public release" so it states what is true (tagged on GitHub, not yet published to npm). Leave other docs alone.

## Acceptance
- The first install instruction in the README Quick start is the build-from-source path.
- No README line presents `npm install -g agent-communication-system` or `npx -p agent-communication-system` as something that works now.
- `CHANGELOG.md` no longer claims a public npm release that does not exist.
