// I02: tests/wait-notify.test.ts no longer asserts bare wall-clock bounds, keeps every test, and passes 20x in a row under CPU contention.
import { spawn } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";
import { BASE_SHA, CHECKOUT, check, killGroup, read, sh } from "./lib.mjs";

const file = "tests/wait-notify.test.ts";
const source = read(file);
check(source !== null, `${file} is missing`);
const literalBound = source.match(/assert\.ok\([^;\n]*<\s*\d+\s*[,)]/);
check(!literalBound, `${file} still asserts a literal bound: ${literalBound?.[0]}`);
check(!/\.(skip|todo|only)\(|\{[^}]*\b(skip|todo|only)\s*:\s*true/.test(source), `${file} skips, todos or isolates a test`);
const count = (text) => (text.match(/^test\(/gm) ?? []).length;
const before = sh("git", ["show", `${BASE_SHA}:${file}`]);
check(before.code === 0, `cannot read the base version of ${file}`);
check(count(source) >= count(before.stdout), `${file} has fewer tests than the base (${count(source)} < ${count(before.stdout)})`);

const compiled = sh("npm", ["run", "test:compile"]);
check(compiled.code === 0, `test:compile failed:\n${compiled.stdout.slice(-1500)}`);
mkdirSync(join(CHECKOUT, "dist-test"), { recursive: true });
copyFileSync(join(CHECKOUT, "tests/fixtures/test-bus.config.json"), join(CHECKOUT, "dist-test/agent-bus.config.json"));

const pin = sh("taskset", ["-c", "0", "true"]).code === 0 ? ["taskset", "-c", "0"] : [];
const withPin = (args) => [...pin, process.execPath, ...args];
const burners = [0, 1].map(() => {
  const argv = withPin(["-e", "for(;;){}"]);
  return spawn(argv[0], argv.slice(1), { detached: true, stdio: "ignore" });
});
try {
  for (let i = 1; i <= 20; i += 1) {
    const argv = withPin(["--test", "--test-concurrency=1", "--test-force-exit", "dist-test/tests/wait-notify.test.js"]);
    const result = sh(argv[0], argv.slice(1));
    check(result.code === 0, `run ${i}/20 of ${file} failed under CPU contention:\n${(result.stdout + result.stderr).slice(-1500)}`);
  }
} finally {
  burners.forEach((child) => killGroup(child));
}
