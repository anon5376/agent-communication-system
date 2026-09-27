import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";
import { loadConfig, validateConfig } from "../src/config.js";
import { EMPTY_BUS_CONFIG } from "../src/provider-catalog.js";

test("production config starts empty with no fake catalog or stock agents", () => {
  const config = loadConfig(join(process.cwd(), "agent-bus.config.json"));
  assert.deepEqual(Object.keys(config.providers), []);
  assert.deepEqual(Object.keys(config.harnesses), []);
  assert.deepEqual(Object.keys(config.models), []);
  assert.deepEqual(Object.keys(config.agents), []);
  assert.ok(config.roles.manager);
  const raw = readFileSync(join(process.cwd(), "agent-bus.config.json"), "utf8");
  assert.doesNotMatch(raw, /fake-small|fake-strong|"fake"|opus-current|"opus"|"gpt"|"kimi"|"gem"/);
  validateConfig(EMPTY_BUS_CONFIG);
});
