import assert from "node:assert/strict";
import { chmodSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";
import { EMPTY_BUS_CONFIG } from "../src/provider-catalog.js";
import { applyFoundProviders, mergeCatalogProvider, resolveBinary, scanProviders } from "../src/discover.js";
import { routeTask } from "../src/router.js";
import { testConfig, temporaryDirectory } from "./helpers.js";

test("catalog merge adds Cursor as an ordinary provider", () => {
  const config = mergeCatalogProvider(structuredClone(EMPTY_BUS_CONFIG), "cursor", { command: "/tmp/cursor-agent", enabled: true });
  assert.equal(config.providers.cursor.enabled, true);
  assert.equal(config.harnesses.cursor.adapter, "cursor");
  assert.equal(config.harnesses.cursor.command, "/tmp/cursor-agent");
  assert.equal(config.models["cursor-default"].provider, "cursor");
  assert.equal(Object.keys(config.agents).length, 0);
});

test("applyFoundProviders never enables a missing CLI", () => {
  const { config, added } = applyFoundProviders(structuredClone(EMPTY_BUS_CONFIG), [{
    id: "anthropic",
    displayName: "Anthropic",
    authKind: "subscription",
    authSource: "x",
    loginCommand: "claude auth login",
    installHint: "",
    harnessId: "claude",
    adapter: "claude",
    binaries: ["claude"],
    configured: false,
    enabled: false,
    cliFound: false,
    resolvedPath: null,
    version: null,
    error: "missing",
    command: "claude",
    discoveredModels: [],
  }]);
  assert.deepEqual(added, []);
  assert.equal(config.providers.anthropic, undefined);
});

test("resolveBinary finds an explicit executable path", () => {
  const root = temporaryDirectory("agent-bus-bin-");
  const bin = join(root, "cursor-agent");
  writeFileSync(bin, "#!/bin/sh\necho ok\n");
  chmodSync(bin, 0o755);
  assert.equal(resolveBinary("cursor-agent", [], bin), bin);
  assert.equal(resolveBinary("definitely-not-installed-agent-bus"), null);
});

test("scanProviders lists catalog entries even on an empty bus", async () => {
  const rows = await scanProviders(structuredClone(EMPTY_BUS_CONFIG));
  assert.ok(rows.some((row) => row.id === "cursor"));
  assert.ok(rows.some((row) => row.id === "anthropic"));
  assert.ok(rows.some((row) => row.id === "xai"));
  assert.ok(!rows.some((row) => row.id === "fake"));
});

test("allowedAgentIds blocks managers from spawning unlisted agents", () => {
  const config = testConfig();
  const decision = routeTask(config, {
    role: "cheap-worker",
    complexity: 1,
    contextTokens: 1000,
    writeAccess: true,
    shell: true,
    network: false,
    allowedAgentIds: ["fake-small"],
  });
  assert.equal(decision.selectedAgentId, "fake-small");
  const blocked = routeTask(config, {
    role: "manager",
    complexity: 3,
    contextTokens: 1000,
    writeAccess: true,
    shell: true,
    network: false,
    allowedAgentIds: ["fake-small"],
  });
  assert.equal(blocked.selectedAgentId, null);
});
