# Releasing

Checklist for cutting a release. Current state: no tags exist yet — the first
tagged release is `v0.2.0`.

## 1. Pre-flight

```sh
npm run build && npm run test:compile && node --test dist-test/tests/*.test.js
npm audit --audit-level=high
npm run audit:public            # worktree has no private data
npm run audit:public:history    # git history has no private data — required before publishing
git diff --check
```

CI must be green on `main`.

## 2. Version

- Bump `"version"` in `package.json` (semver: features = minor, fixes = patch).
- Add a `CHANGELOG.md` section for the new version with dated entries.

## 3. npm publish

Requires PR #4 (publishable package: `private` dropped, `files` whitelist,
`publishConfig`, `prepack` build) to be merged first.

```sh
npm login          # once, owner account
npm pack           # inspect the tarball contents
npm publish        # private:false, access public (set by publishConfig)
npx -y agent-communication-system@latest --help   # smoke the published artifact (works only after the first publish)
```

After the first publish, rewrite the README Quick start to lead with `npm install -g`, drop the "After the first npm release" paragraph, and set `ACS_CHECK_NPM=1` for the `audit:public` step in CI; `scripts/check-readme-claims.mjs` otherwise keeps failing on the npm command.

## 4. Tag + GitHub release

```sh
git tag -a v<version> -m "v<version>"
git push origin v<version>
```

Then create the GitHub Release on the tag (releases page → Draft new release →
Generate notes + paste the CHANGELOG section). Attach assets as desired:

- `npm pack` tarball (`agent-communication-system-<version>.tgz`)
- Rust binaries — built from the `rust-port` branch:
  `cargo build --release --manifest-path rust/Cargo.toml` produces `qagent`
  and `acs` (~4 MB each); name them `<bin>-<version>-<target>` before upload.

## 5. Post-release

- Official MCP Registry + registries sweep — `docs/submission-pack.md` has the
  exact submission text for all 11 targets (on the promotion-research branch).
- Launch copy — `docs/launch-copy.md` (Show HN / Reddit / X drafts).
