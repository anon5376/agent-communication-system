import { existsSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { delimiter, isAbsolute, join } from "node:path";
import { probeCommand } from "./adapters.js";
import { BusConfig } from "./config.js";
import {
  PROVIDER_CATALOG,
  ProviderCatalogEntry,
  catalogEntry,
  catalogHarnessDefinition,
  catalogModelDefinition,
  catalogProviderDefinition,
} from "./provider-catalog.js";

export interface ProviderScan {
  id: string;
  displayName: string;
  authKind: string;
  authSource: string;
  loginCommand: string;
  installHint: string;
  apiKeyEnv?: string;
  harnessId: string;
  adapter: string;
  binaries: string[];
  configured: boolean;
  enabled: boolean;
  cliFound: boolean;
  resolvedPath: string | null;
  version: string | null;
  error: string | null;
  command: string;
  discoveredModels: string[];
}

function expandHome(path: string): string {
  if (path.startsWith("~/")) return join(homedir(), path.slice(2));
  if (path === "~") return homedir();
  return path;
}

function isExecutableFile(path: string): boolean {
  try {
    const stat = statSync(path);
    return stat.isFile() && (stat.mode & 0o111) !== 0;
  } catch {
    return false;
  }
}

function pathDirs(): string[] {
  const extra = [
    join(homedir(), ".local", "bin"),
    "/opt/homebrew/bin",
    "/usr/local/bin",
    join(homedir(), "bin"),
    join(homedir(), ".cursor", "bin"),
  ];
  const fromPath = (process.env.PATH ?? "").split(delimiter).filter(Boolean);
  return [...new Set([...fromPath, ...extra])];
}

export function resolveBinary(command: string, knownPaths: string[] = [], explicit?: string): string | null {
  if (explicit?.trim()) {
    const path = expandHome(explicit.trim());
    if (existsSync(path)) return path;
    return null;
  }
  if (isAbsolute(command) && existsSync(command)) return command;
  for (const dir of pathDirs()) {
    const candidate = join(dir, command);
    if (isExecutableFile(candidate) || existsSync(candidate)) return candidate;
  }
  for (const known of knownPaths) {
    const candidate = expandHome(known);
    if (existsSync(candidate)) return candidate;
  }
  return null;
}

async function probeEntry(entry: ProviderCatalogEntry, explicitCommand?: string): Promise<{
  cliFound: boolean;
  resolvedPath: string | null;
  version: string | null;
  error: string | null;
  command: string;
}> {
  const binaries = entry.binaries;
  if (explicitCommand?.trim()) {
    const resolved = resolveBinary(explicitCommand.trim(), [], explicitCommand.trim());
    const command = resolved ?? explicitCommand.trim();
    const probe = await probeCommand(command, binaries[0]?.probeArgs ?? ["--version"]);
    return {
      cliFound: probe.available,
      resolvedPath: resolved,
      version: probe.version,
      error: probe.error,
      command,
    };
  }
  for (const binary of binaries) {
    const resolved = resolveBinary(binary.command, binary.knownPaths);
    if (!resolved) continue;
    const probe = await probeCommand(resolved, binary.probeArgs);
    if (probe.available) {
      return { cliFound: true, resolvedPath: resolved, version: probe.version, error: null, command: resolved };
    }
  }
  const first = binaries[0];
  return {
    cliFound: false,
    resolvedPath: null,
    version: null,
    error: first ? `${first.command} not found on PATH or known install locations` : "no binary configured",
    command: first?.command ?? entry.harnessId,
  };
}

export async function scanProviders(config: BusConfig, overrides: Record<string, string> = {}): Promise<ProviderScan[]> {
  const rows: ProviderScan[] = [];
  const seen = new Set<string>();
  for (const entry of PROVIDER_CATALOG) {
    seen.add(entry.id);
    const existing = config.providers[entry.id];
    const harness = config.harnesses[entry.harnessId];
    const explicit = overrides[entry.id] ?? (harness?.command && isAbsolute(harness.command) ? harness.command : undefined);
    const probe = await probeEntry(entry, explicit);
    rows.push({
      id: entry.id,
      displayName: entry.displayName,
      authKind: entry.authKind,
      authSource: entry.authSource,
      loginCommand: existing?.loginCommand ?? entry.loginCommand,
      installHint: existing?.installHint ?? entry.installHint,
      apiKeyEnv: existing?.apiKeyEnv ?? entry.apiKeyEnv,
      harnessId: entry.harnessId,
      adapter: entry.adapter,
      binaries: entry.binaries.map((item) => item.command),
      configured: Boolean(existing),
      enabled: Boolean(existing?.enabled && harness?.enabled),
      cliFound: probe.cliFound,
      resolvedPath: probe.resolvedPath,
      version: probe.version,
      error: probe.error,
      command: harness?.command ?? probe.command,
      discoveredModels: [],
    });
  }
  for (const provider of Object.values(config.providers)) {
    if (seen.has(provider.id)) continue;
    const harness = Object.values(config.harnesses).find((item) => item.providers.includes(provider.id));
    const command = harness?.command ?? "";
    const probe = command ? await probeCommand(command, harness?.probeArgs ?? ["--version"]) : { available: false, version: null, error: "no harness" };
    rows.push({
      id: provider.id,
      displayName: provider.displayName,
      authKind: provider.authKind,
      authSource: provider.authSource,
      loginCommand: provider.loginCommand ?? "",
      installHint: provider.installHint ?? "",
      apiKeyEnv: provider.apiKeyEnv,
      harnessId: harness?.id ?? "",
      adapter: harness?.adapter ?? "command",
      binaries: harness?.command ? [harness.command] : [],
      configured: true,
      enabled: Boolean(provider.enabled && harness?.enabled),
      cliFound: probe.available,
      resolvedPath: command && existsSync(command) ? command : null,
      version: probe.version,
      error: probe.error,
      command,
      discoveredModels: [],
    });
  }
  return rows;
}

export function mergeCatalogProvider(
  config: BusConfig,
  providerId: string,
  options: { command?: string; enabled?: boolean } = {},
): BusConfig {
  const entry = catalogEntry(providerId);
  if (!entry) throw new Error(`unknown catalog provider: ${providerId}`);
  const enabled = options.enabled !== false;
  const command = options.command?.trim() || config.harnesses[entry.harnessId]?.command || entry.binaries[0]?.command;
  if (!command) throw new Error(`no command for provider ${providerId}`);

  const next = structuredClone(config);
  const existingProvider = next.providers[providerId];
  next.providers[providerId] = {
    ...catalogProviderDefinition(entry, enabled),
    ...existingProvider,
    id: providerId,
    displayName: existingProvider?.displayName ?? entry.displayName,
    enabled,
  };
  if (entry.id === "ollama") return next;
  const existingHarness = next.harnesses[entry.harnessId];
  const harness = catalogHarnessDefinition(entry, command, enabled);
  if (existingHarness) {
    next.harnesses[entry.harnessId] = {
      ...existingHarness,
      command,
      enabled: enabled ? true : existingHarness.enabled,
      providers: [...new Set([...existingHarness.providers, providerId])],
    };
  } else {
    next.harnesses[entry.harnessId] = harness;
  }
  for (const seed of entry.models) {
    if (next.models[seed.id]) {
      if (enabled) next.models[seed.id].enabled = true;
      continue;
    }
    next.models[seed.id] = catalogModelDefinition(entry, seed, enabled);
  }
  return next;
}

export function applyFoundProviders(config: BusConfig, scans: ProviderScan[]): { config: BusConfig; added: string[] } {
  let next = structuredClone(config);
  const added: string[] = [];
  for (const scan of scans) {
    if (!scan.cliFound || scan.id === "ollama") continue;
    const wasEnabled = Boolean(next.providers[scan.id]?.enabled && next.harnesses[scan.harnessId]?.enabled);
    next = mergeCatalogProvider(next, scan.id, {
      command: scan.resolvedPath ?? scan.command,
      enabled: true,
    });
    if (!wasEnabled) added.push(scan.id);
  }
  return { config: next, added };
}
