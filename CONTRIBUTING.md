# Contributing

Contributions should be small, testable, and free of private project data. Open an issue before adding a dependency or changing the protocol, database schema, identity model, or compatibility aliases.

## Local checks

```bash
npm ci
npm run audit:public
npm run build
npm run test:unit
npm run test:lifecycle
npm run test:browser
npm run check
npm audit --audit-level=high
git diff --check
```

`npm run audit:public:history` is required before publishing a branch or release from a repository whose history may contain credentials, personal paths, or personal commit metadata.

## Pull requests

State the problem, the smallest behavior change, the tests you ran, and any known limitation. Do not include bus databases, logs, tokens, provider credentials, private project names, absolute home-directory paths, or generated material copied from another project.

## Security reports

Use [GitHub Security Advisories](https://github.com/anon5376/agent-communication-system/security/advisories/new) for vulnerabilities. Do not open a public issue containing a secret, exploit, private path, or user data.
