// I12: with constraints.optionalApiCostBudgetUSD set, the supervisor stops starting turns once the cost reported so far reaches it, and writes a `budget_exceeded` event.
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { QAGENT, check, fixtureConfig, killGroup, newBus, spawnChild, until } from "./lib.mjs";

const bus = newBus();
let supervisor;
try {
  bus.json(null, ["agent", "add", "fake-small", "--role", "cheap-worker", "--harness", "fake"]);
  const project = join(bus.home, "project");
  mkdirSync(project);
  const reply = JSON.stringify({ result: "done", usage: { inputTokens: 10, outputTokens: 5, totalTokens: 15, costUSD: 0.02 } });
  const configPath = join(bus.home, "config.json");
  writeFileSync(configPath, JSON.stringify(fixtureConfig((config) => {
    config.harnesses.fake.adapter = "command";
    config.harnesses.fake.command = process.execPath;
    config.harnesses.fake.features.mcp = false;
    config.agents["fake-small"].harnessOptions = { args: ["-e", `process.stdout.write(${JSON.stringify(reply)} + "\\n")`] };
    config.constraints.optionalApiCostBudgetUSD = 0.01;
  })));
  supervisor = spawnChild(process.execPath, [QAGENT, "supervise", "fake-small", project, "--config", configPath], { env: bus.env() });
  await until("the supervisor to hold its wait", 20_000, () => bus.json(null, ["agent", "list"]).find((agent) => agent.id === "fake-small")?.status === "waiting");

  const first = bus.json(null, ["task", "add", "First, within budget", "--to", "fake-small"]);
  await until("the first task to be submitted", 30_000, () => bus.json(null, ["task", "show", String(first.id)]).state === "submitted");
  const second = bus.json(null, ["task", "add", "Second, over budget", "--to", "fake-small"]);
  const exceeded = await until("a budget_exceeded event", 30_000, () => bus.cli(null, ["log", "--json", "--limit", "1000"]).stdout.split("\n").filter(Boolean).map((line) => JSON.parse(line)).find((event) => event.kind === "budget_exceeded"));
  check(exceeded, "no budget_exceeded event");
  await new Promise((resolve) => setTimeout(resolve, 6_000));
  const after = bus.json(null, ["task", "show", String(second.id)]);
  check(after.state === "open" && after.assignee === "fake-small", `the second task must not be started once the cap is reached; it is ${after.state}`);
} finally {
  killGroup(supervisor);
  bus.cleanup();
}
