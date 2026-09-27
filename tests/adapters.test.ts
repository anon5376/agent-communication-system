import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";
import { AdapterContext, getHarnessAdapter } from "../src/adapters.js";
import { ResolvedAgent, resolveAgent } from "../src/config.js";
import { mergeCatalogProvider } from "../src/discover.js";
import { resumedUnexpectedSession, retryDelayMs, runHarnessProcess } from "../src/supervisor.js";
import { temporaryDirectory, testConfig } from "./helpers.js";

function contextFor(agentId: string): AdapterContext {
  const config = testConfig();
  const agent = resolveAgent(config, agentId);
  return {
    agent,
    prompt: "perform a deterministic test",
    sessionId: null,
    pinnedSessionId: null,
    workdir: temporaryDirectory(),
    mcpServerPath: "/tmp/mcp-server.js",
    fakeHarnessPath: join(process.cwd(), "dist", "fake-harness.js"),
    busEnvironment: { AGENT_TOKEN: "secret-test-token" },
  };
}

test("Claude adapter normalizes JSON envelope and usage", () => {
  const config = testConfig();
  config.providers.anthropic.enabled = true;
  config.harnesses.claude.enabled = true;
  config.models["opus-current"].enabled = true;
  config.agents.opus.enabled = true;
  const agent = resolveAgent(config, "opus");
  const adapter = getHarnessAdapter("claude");
  const invocation = adapter.build({
    agent,
    prompt: "test",
    sessionId: null,
    pinnedSessionId: null,
    workdir: temporaryDirectory(),
    mcpServerPath: "/tmp/mcp.js",
    fakeHarnessPath: "/tmp/fake.js",
    busEnvironment: { AGENT_TOKEN: "token" },
  });
  assert.ok(invocation.args.includes("--mcp-config"));
  assert.ok(invocation.args.includes("--output-format"));
  const parsed = adapter.parse(JSON.stringify({
    result: "done",
    session_id: "session-1",
    usage: { input_tokens: 10, output_tokens: 5, cache_read_input_tokens: 2 },
    total_cost_usd: 0.1,
  }), 0);
  assert.equal(parsed.text, "done");
  assert.equal(parsed.sessionId, "session-1");
  assert.equal(parsed.usage.totalTokens, 17);
  assert.equal(parsed.usage.costUSD, 0.1);
});

test("Codex adapter places options before positional prompt", () => {
  const config = testConfig();
  config.providers.openai.enabled = true;
  config.harnesses.codex.enabled = true;
  config.models["codex-default"].enabled = true;
  config.agents.gpt.enabled = true;
  const agent = resolveAgent(config, "gpt");
  const invocation = getHarnessAdapter("codex").build({
    agent,
    prompt: "THE_PROMPT",
    sessionId: null,
    pinnedSessionId: null,
    workdir: temporaryDirectory(),
    mcpServerPath: "/tmp/mcp.js",
    fakeHarnessPath: "/tmp/fake.js",
    busEnvironment: { AGENT_TOKEN: "token" },
  });
  assert.equal(invocation.args.at(-1), "THE_PROMPT");
  assert.ok(invocation.args.indexOf("-c") < invocation.args.indexOf("THE_PROMPT"));
  assert.ok(invocation.args.includes("--dangerously-bypass-approvals-and-sandbox"));
  assert.ok(invocation.args.includes("--json"));
});

test("Codex adapter resumes an exact managed session instead of latest", () => {
  const context = contextFor("gpt");
  context.sessionId = "019ff1f7-cfea-7240-a6fe-f1ab2cb2fe4a";
  const invocation = getHarnessAdapter("codex").build(context);
  assert.deepEqual(invocation.args.slice(0, 2), ["exec", "resume"]);
  assert.ok(invocation.args.includes(context.sessionId));
  assert.ok(!invocation.args.includes("--last"));
});

test("Codex adapter queues pinned mail into the exact original task", () => {
  const context = contextFor("gpt");
  context.sessionId = "019ff1f7-cfea-7240-a6fe-f1ab2cb2fe4a";
  context.pinnedSessionId = context.sessionId;
  const invocation = getHarnessAdapter("codex").build(context);
  assert.equal(invocation.args[0], "queue");
  assert.equal(invocation.args[invocation.args.indexOf("--thread") + 1], context.sessionId);
  assert.equal(invocation.args[invocation.args.indexOf("--message") + 1], context.prompt);
});

test("Codex adapter captures the exact session from JSON events", () => {
  const parsed = getHarnessAdapter("codex").parse([
    JSON.stringify({ type: "thread.started", thread_id: "thread-exact" }),
    JSON.stringify({ type: "item.completed", item: { type: "agent_message", text: "done" } }),
    JSON.stringify({ type: "turn.completed", usage: { input_tokens: 11, output_tokens: 4 } }),
  ].join("\n"), 0);
  assert.equal(parsed.sessionId, "thread-exact");
  assert.equal(parsed.text, "done");
  assert.equal(parsed.usage.totalTokens, 15);
});

test("fake adapter runs a normalized successful process", async () => {
  const context = contextFor("fake-small");
  const adapter = getHarnessAdapter("fake");
  const invocation = adapter.build(context);
  const processResult = await runHarnessProcess(invocation, context.agent, context.workdir);
  const normalized = adapter.parse(processResult.output, processResult.code);
  assert.equal(processResult.code, 0);
  assert.equal(normalized.malformed, false);
  assert.match(normalized.text, /completed/);
  assert.ok(normalized.usage.totalTokens > 0);
});

test("malformed provider output is detected", () => {
  const parsed = getHarnessAdapter("fake").parse("not-json", 0);
  assert.equal(parsed.malformed, true);
});

test("Cursor adapter uses print/json/force and writes MCP config", () => {
  const config = mergeCatalogProvider(testConfig(), "cursor", { command: "cursor-agent", enabled: true });
  config.providers.cursor.enabled = true;
  config.harnesses.cursor.enabled = true;
  config.models["cursor-default"].enabled = true;
  config.agents["cursor-worker"] = {
    id: "cursor-worker",
    model: "cursor-default",
    role: "implementation",
    authority: "worker",
    description: "Cursor CLI worker",
    enabled: true,
    autoStart: false,
    permissions: { canDelegate: false, canReview: false, filesystem: "write", shell: true, network: true, maxDelegationDepth: 0, allowedPaths: ["."] },
  };
  const agent = resolveAgent(config, "cursor-worker");
  const workdir = temporaryDirectory();
  const adapter = getHarnessAdapter("cursor");
  adapter.prepare?.({
    agent,
    prompt: "test",
    sessionId: null,
    pinnedSessionId: null,
    workdir,
    mcpServerPath: "/tmp/mcp.js",
    fakeHarnessPath: "/tmp/fake.js",
    busEnvironment: { AGENT_TOKEN: "token" },
  });
  const invocation = adapter.build({
    agent,
    prompt: "test",
    sessionId: "chat-1",
    pinnedSessionId: null,
    workdir,
    mcpServerPath: "/tmp/mcp.js",
    fakeHarnessPath: "/tmp/fake.js",
    busEnvironment: { AGENT_TOKEN: "token" },
  });
  assert.equal(invocation.command, "cursor-agent");
  assert.ok(invocation.args.includes("-p"));
  assert.ok(invocation.args.includes("--output-format"));
  assert.ok(invocation.args.includes("json"));
  assert.ok(invocation.args.includes("--force"));
  assert.ok(invocation.args.includes("--resume"));
  const mcp = JSON.parse(readFileSync(join(workdir, ".cursor", "mcp.json"), "utf8"));
  assert.equal(mcp.mcpServers["qagent"].args[0], "/tmp/mcp.js");
});

function providerAgent(providerId: string, agentId: string): ResolvedAgent {
  let config = mergeCatalogProvider(testConfig(), providerId, { enabled: true });
  const model = Object.values(config.models).find((item) => item.provider === providerId);
  assert.ok(model, `catalog model for ${providerId}`);
  config.agents[agentId] = {
    id: agentId,
    model: model.id,
    role: "implementation",
    authority: "worker",
    description: `${providerId} adapter test`,
    enabled: true,
    autoStart: false,
    permissions: { canDelegate: false, canReview: false, filesystem: "write", shell: true, network: true, maxDelegationDepth: 0 },
  };
  return resolveAgent(config, agentId);
}

test("native adapters use each installed harness's exact resume syntax", () => {
  const cases = [
    { provider: "anthropic", adapter: "claude", flag: "--resume" },
    { provider: "cursor", adapter: "cursor", flag: "--resume" },
    { provider: "moonshot", adapter: "kimi", flag: "--session" },
    { provider: "google", adapter: "gemini", flag: "--resume" },
    { provider: "xai", adapter: "grok", flag: "-r" },
    { provider: "opencode", adapter: "opencode", flag: "-s" },
    { provider: "novita", adapter: "hermes", flag: "--resume" },
    { provider: "zai", adapter: "opencode", flag: "-s" },
  ];
  for (const row of cases) {
    const agent = providerAgent(row.provider, `${row.provider}-worker`);
    const invocation = getHarnessAdapter(row.adapter).build({
      agent,
      prompt: "wake on bus mail",
      sessionId: "exact-native-session",
      pinnedSessionId: "exact-native-session",
      workdir: temporaryDirectory(),
      mcpServerPath: "/tmp/mcp.js",
      fakeHarnessPath: "/tmp/fake.js",
      busEnvironment: { AGENT_TOKEN: "token" },
    });
    assert.equal(invocation.args[invocation.args.indexOf(row.flag) + 1], "exact-native-session", `${row.adapter} resume flag`);
  }
});

test("generic command adapter supports a separate resume command", () => {
  const config = testConfig();
  config.harnesses.fake.adapter = "command";
  config.agents["fake-small"].harnessOptions = {
    args: ["new", "{prompt}"],
    resumeArgs: ["resume", "--chat", "{session}", "{prompt}"],
  };
  const agent = resolveAgent(config, "fake-small");
  const invocation = getHarnessAdapter("command").build({
    ...contextFor("fake-small"),
    agent,
    sessionId: "custom-session",
    pinnedSessionId: "custom-session",
  });
  assert.deepEqual(invocation.args, ["resume", "--chat", "custom-session", "perform a deterministic test"]);
});

test("an optional mcpCommand replaces the MCP launch line in every MCP-configuring adapter", () => {
  const mcpCommand = { command: "/usr/bin/node", args: ["/opt/qagent/dist/qagent.js", "mcp"], env: { QAGENT_AGENT_ID: "worker-1", QAGENT_BUS_DB: "/tmp/bus.db" } };
  const claudeInvocation = getHarnessAdapter("claude").build({ ...contextFor("fake-small"), agent: providerAgent("anthropic", "claude-worker"), mcpCommand });
  const claudeServer = JSON.parse(claudeInvocation.args[claudeInvocation.args.indexOf("--mcp-config") + 1]).mcpServers.qagent;
  assert.equal(claudeServer.command, "/usr/bin/node");
  assert.deepEqual(claudeServer.args, mcpCommand.args);
  assert.equal(claudeServer.env.QAGENT_AGENT_ID, "worker-1");
  assert.equal(claudeServer.env.AGENT_ID, "claude-worker");

  const codex = getHarnessAdapter("codex").build({ ...contextFor("gpt"), mcpCommand }).args;
  assert.ok(codex.includes(`mcp_servers.agent_bus.args=${JSON.stringify(mcpCommand.args)}`));
  assert.ok(codex.some((arg) => arg.startsWith("mcp_servers.agent_bus.env=") && arg.includes("\"QAGENT_AGENT_ID\":\"worker-1\"")));

  const context = { ...contextFor("fake-small"), agent: providerAgent("opencode", "opencode-worker"), mcpCommand };
  getHarnessAdapter("opencode").prepare?.(context);
  const opencode = JSON.parse(readFileSync(join(context.workdir, "opencode.json"), "utf8")).mcp.qagent;
  assert.deepEqual(opencode.command, ["/usr/bin/node", ...mcpCommand.args]);
  assert.equal(opencode.environment.QAGENT_BUS_DB, "/tmp/bus.db");

  const fake = getHarnessAdapter("fake").build({ ...contextFor("fake-small"), mcpCommand });
  assert.deepEqual(JSON.parse(fake.environment.QAGENT_MCP_COMMAND), { ...mcpCommand, env: mcpCommand.env });
  assert.equal(getHarnessAdapter("fake").build(contextFor("fake-small")).environment.QAGENT_MCP_COMMAND, undefined);
  const legacy = getHarnessAdapter("codex").build(contextFor("gpt")).args;
  assert.ok(legacy.includes("mcp_servers.agent_bus.args=[\"/tmp/mcp-server.js\"]"), "without mcpCommand the legacy line is unchanged");
});

test("supervisor rejects a harness that silently forks a pinned session", () => {
  assert.equal(resumedUnexpectedSession("expected", "forked"), true);
  assert.equal(resumedUnexpectedSession("expected", "expected"), false);
  assert.equal(resumedUnexpectedSession("expected", null), false);
});

test("supervisor retry backoff is bounded and exponential", () => {
  assert.equal(retryDelayMs(1), 2_000);
  assert.equal(retryDelayMs(2), 4_000);
  assert.equal(retryDelayMs(99), 60_000);
});
