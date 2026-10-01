#!/usr/bin/env node
// Fails when the README claims something the repository does not back:
//  - an npm install command for this package outside the "After the first npm release" paragraph
//    (set ACS_CHECK_NPM=1 to skip that check once the registry returns the package);
//  - a harness adapter list that differs from ADAPTERS in src/adapters.ts.

import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const ROOT = resolve(process.cwd());
const readme = readFileSync(resolve(ROOT, "README.md"), "utf8");
const adaptersSource = readFileSync(resolve(ROOT, "src/adapters.ts"), "utf8");
const problems = [];

async function published() {
  if (process.env.ACS_CHECK_NPM !== "1") return false;
  try {
    const response = await fetch("https://registry.npmjs.org/agent-communication-system", { signal: AbortSignal.timeout(10_000) });
    return response.status === 200;
  } catch {
    return false;
  }
}

if (!(await published())) {
  const installCommand = /npm install -g agent-communication-system|npx (?:-y )?-p agent-communication-system/;
  const note = readme.indexOf("After the first npm release");
  const first = readme.search(installCommand);
  if (first !== -1 && (note === -1 || first < note)) {
    problems.push("README shows an npm install command for agent-communication-system before the 'After the first npm release' note, but the package is not published (check with ACS_CHECK_NPM=1)");
  }
}

const block = adaptersSource.match(/const ADAPTERS\b[^=]*=\s*\{([^}]*)\}/);
if (!block) {
  problems.push("could not find the ADAPTERS table in src/adapters.ts");
} else {
  const shipped = [...block[1].matchAll(/^\s*(\w+):/gm)].map((match) => match[1]);
  const line = readme.split("\n").find((entry) => entry.startsWith("- Harness adapters"));
  if (!line) {
    problems.push("README has no '- Harness adapters' line");
  } else {
    const named = [...line.matchAll(/`(\w+)`/g)].map((match) => match[1]);
    const expected = shipped.filter((id) => id !== "fake");
    for (const id of named) if (!shipped.includes(id)) problems.push(`README names adapter '${id}' that is not in ADAPTERS`);
    for (const id of expected) if (!named.includes(id)) problems.push(`README omits adapter '${id}' from ADAPTERS`);
  }
}

if (problems.length) {
  for (const problem of problems) console.error(`readme-claims: ${problem}`);
  process.exit(1);
}
console.log("readme-claims: ok");
