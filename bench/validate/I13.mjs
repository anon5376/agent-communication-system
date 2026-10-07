// I13: `qagent trust --json` prints accepted/total review verdicts per (assignee model, task role), matching a hand-computed bus.
import { check, newBus } from "./lib.mjs";

const bus = newBus();
try {
  bus.json(null, ["agent", "add", "lead", "--role", "manager", "--authority", "manager"]);
  bus.json(null, ["agent", "add", "a1", "--role", "worker", "--model", "m1"]);
  bus.json(null, ["agent", "add", "a2", "--role", "worker", "--model", "m2"]);
  const work = (agent, role, verdicts) => {
    const task = bus.json("lead", ["task", "add", `${role} task`, "--to", agent, "--role", role]);
    for (const verdict of verdicts) {
      bus.json(agent, ["task", "claim", String(task.id)]);
      bus.json(agent, ["task", "submit", String(task.id), "--summary", "work"]);
      bus.json("lead", ["task", "review", String(task.id), verdict === "accept" ? "--accept" : "--revise", "--feedback", verdict]);
    }
  };
  work("a1", "impl", ["accept"]);
  work("a1", "impl", ["revise", "accept"]);
  work("a2", "impl", ["accept"]);
  work("a2", "research", ["revise", "revise", "revise"]);
  work("a1", "research", ["accept"]);
  const rows = bus.json(null, ["trust"]);
  check(Array.isArray(rows), "trust --json must print an array");
  const key = (row) => `${row.model}/${row.role}`;
  const got = Object.fromEntries(rows.map((row) => [key(row), `${row.accepted}/${row.total}`]));
  const want = { "m1/impl": "2/3", "m1/research": "1/1", "m2/impl": "1/1", "m2/research": "0/3" };
  check(JSON.stringify(Object.keys(got).sort()) === JSON.stringify(Object.keys(want).sort()), `groups differ: got ${JSON.stringify(got)}, want ${JSON.stringify(want)}`);
  for (const [name, value] of Object.entries(want)) check(got[name] === value, `${name}: want ${value}, got ${got[name]}`);
  const text = bus.cli(null, ["trust"]);
  check(text.code === 0 && text.stdout.includes("m1") && text.stdout.includes("2/3"), "the text form must show model and accepted/total");
} finally {
  bus.cleanup();
}
