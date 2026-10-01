// I08: a kill -9 recovery test exists (SIGKILL of a supervised turn, stalled detection, requeue, second supervisor completes) and passes.
import { copyFileSync, mkdirSync, readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { CHECKOUT, check, sh } from "./lib.mjs";

const candidates = readdirSync(join(CHECKOUT, "tests")).filter((name) => name.endsWith(".test.ts")).filter((name) => {
  const text = readFileSync(join(CHECKOUT, "tests", name), "utf8");
  return /SIGKILL/.test(text) && /supervise/.test(text) && /requeue/i.test(text) && /\b(test|it)\s*\(\s*["'`][^"'`]*\b(kill|crash)/i.test(text);
});
check(candidates.length > 0, "no test combines SIGKILL, supervise and requeue under a test title that mentions kill or crash");
const compiled = sh("npm", ["run", "test:compile"]);
check(compiled.code === 0, `test:compile failed:\n${compiled.stdout.slice(-1500)}`);
mkdirSync(join(CHECKOUT, "dist-test"), { recursive: true });
copyFileSync(join(CHECKOUT, "tests/fixtures/test-bus.config.json"), join(CHECKOUT, "dist-test/agent-bus.config.json"));
for (const name of candidates) {
  const result = sh(process.execPath, ["--test", "--test-concurrency=1", "--test-force-exit", `dist-test/tests/${name.replace(/\.ts$/, ".js")}`]);
  check(result.code === 0, `${name} failed:\n${(result.stdout + result.stderr).slice(-1500)}`);
  check(!/# skipped [1-9]/.test(result.stdout), `${name} skipped a test`);
}
