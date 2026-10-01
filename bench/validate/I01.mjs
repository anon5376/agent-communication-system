// I01: the README must not tell users to install an unpublished package as if it worked.
import { check, read } from "./lib.mjs";

const readme = read("README.md");
check(readme !== null, "README.md is missing");
const lines = readme.split("\n");
const install = /npm install -g agent-communication-system|npx -p agent-communication-system/;
const qualifier = /not (yet )?(published|on npm|on the npm registry|available on npm)|unpublished|once (it is )?published|after the first (npm )?release|will be published|404/i;
lines.forEach((line, index) => {
  if (!install.test(line)) return;
  const nearby = lines.slice(Math.max(0, index - 2), index + 3).join("\n");
  check(qualifier.test(nearby), `README.md:${index + 1} presents "${line.trim()}" as available with no note that the package is unpublished`);
});
const first = lines.findIndex((line) => /Quick start/i.test(line));
const section = lines.slice(first).join("\n");
check(/npm ci/.test(section) && /npm run build/.test(section), "the Quick start must show the working path (npm ci, npm run build) from a clone");
const clone = section.search(/npm ci/);
const published = section.search(install);
check(published < 0 || clone < published, "the working source install must come before any unpublished npm command");
const changelog = read("CHANGELOG.md");
check(!/first public release/i.test(changelog ?? ""), "CHANGELOG.md still calls 0.2.0 a first public release");
