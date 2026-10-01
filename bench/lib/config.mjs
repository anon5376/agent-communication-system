// The supervisor configuration for a run, built from the roster in MANIFEST.json.
// Real runs map each roster entry onto the catalog entry of its provider (src/provider-catalog.ts of the build under test);
// fake runs swap every harness for the deterministic fake adapter and keep ids, roles, authorities and families.
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";
import { BenchError } from "./util.mjs";

const PROFILE = { coding: 0.9, reasoning: 0.9, planning: 0.9, debugging: 0.9, research: 0.9, toolUse: 0.9, speed: 0.8, tokenEfficiency: 0.8, reliability: 0.9, autonomy: 0.9, contextTokens: 128000, costClass: "local", source: "heuristic-default" };

function permissions(agent) {
  const manager = agent.authority === "manager";
  const reviewer = manager || agent.role === "reviewer";
  return { canDelegate: manager, canReview: reviewer, filesystem: "write", shell: true, network: true, maxDelegationDepth: manager ? 4 : 0 };
}

async function loadCatalog(qagentBin) {
  const path = join(dirname(qagentBin), "provider-catalog.js");
  if (!existsSync(path)) throw new BenchError(`${path} not found: build the ACS checkout first (npm run build)`);
  return import(pathToFileURL(path).href);
}

function slug(text) {
  return String(text).toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
}

export async function buildConfig({ manifest, qagentBin, fake, isolation, overrides = {} }) {
  const catalog = await loadCatalog(qagentBin);
  const config = structuredClone(catalog.EMPTY_BUS_CONFIG);
  config.constraints.isolation = isolation;
  if (fake) {
    config.providers.fake = { id: "fake", displayName: "Deterministic fake provider", authKind: "local", authSource: "No external authentication", subscriptionBacked: false, enabled: true };
    config.harnesses.fake = {
      id: "fake", adapter: "fake", command: process.execPath, providers: ["fake"], enabled: true, probeArgs: ["--version"],
      features: { headless: true, resume: true, mcp: true, structuredOutput: true, streaming: false, cancellation: true, modelSelection: true, reasoningControl: false, usageReporting: true },
    };
    for (const agent of manifest.roster) {
      const id = `fake-${slug(agent.family)}`;
      config.models[id] ??= { id, provider: "fake", harness: "fake", family: agent.family, exactModel: id, capabilities: { ...PROFILE }, enabled: true };
      config.agents[agent.id] = { id: agent.id, model: id, role: agent.role, authority: agent.authority, description: `fake ${agent.role}`, enabled: true, autoStart: false, permissions: permissions(agent), harnessOptions: { mode: "bus-cli" } };
    }
  } else {
    for (const agent of manifest.roster) {
      const entry = catalog.catalogEntry(agent.provider);
      if (!entry) throw new BenchError(`roster agent ${agent.id}: provider "${agent.provider}" is not in the catalog of ${qagentBin}`);
      const seed = entry.models.find((model) => model.id === agent.model) ?? entry.models[0];
      const modelId = agent.exactModel ? `${seed.id}-${slug(agent.exactModel)}` : seed.id;
      config.providers[entry.id] ??= catalog.catalogProviderDefinition(entry, true);
      config.harnesses[entry.harnessId] ??= catalog.catalogHarnessDefinition(entry, entry.binaries[0].command, true);
      config.models[modelId] ??= catalog.catalogModelDefinition(entry, { ...seed, id: modelId, exactModel: agent.exactModel ?? seed.exactModel }, true);
      config.agents[agent.id] = { id: agent.id, model: modelId, role: agent.role, authority: agent.authority, description: `${agent.role} (${agent.family})`, enabled: true, autoStart: false, permissions: permissions(agent), ...(agent.harnessOptions ? { harnessOptions: agent.harnessOptions } : {}) };
    }
  }
  deepMerge(config, overrides);
  return config;
}

function deepMerge(target, source) {
  for (const [key, value] of Object.entries(source)) {
    if (value && typeof value === "object" && !Array.isArray(value) && target[key] && typeof target[key] === "object") deepMerge(target[key], value);
    else target[key] = value;
  }
}
