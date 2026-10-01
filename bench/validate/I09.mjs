// I09: `qagent preset list --json` prints the four presets ported from the Rust TUI, with charters that match the Rust text.
import { BASE_SHA, CHECKOUT, check, newBus, sh } from "./lib.mjs";

const rust = sh("git", ["show", "origin/rust-port:rust/src/app.rs"]);
check(rust.code === 0, `cannot read origin/rust-port:rust/src/app.rs from the checkout: ${rust.stderr.trim()}`);
const words = (text) => new Set(text.toLowerCase().replace(/\\\n/g, " ").match(/[a-z][a-z-]{2,}/g) ?? []);
const charterOf = (constName) => {
  const match = rust.stdout.match(new RegExp(`const ${constName}: &str = "\\\\\\n([\\s\\S]*?)";`));
  check(match, `cannot find ${constName} in the Rust source`);
  return match[1];
};
const expected = {
  planner: { role: "planner", authority: "worker", charter: charterOf("PLANNER_CHARTER") },
  lead: { role: "orchestrator", authority: "manager", charter: charterOf("LEAD_CHARTER") },
  "worker-hard": { role: "worker-hard", authority: "worker", charter: charterOf("WORKER_HARD_CHARTER") },
  "worker-easy": { role: "worker-easy", authority: "worker", charter: charterOf("WORKER_EASY_CHARTER") },
};
const bus = newBus();
try {
  const presets = bus.json(null, ["preset", "list"]);
  check(Array.isArray(presets) && presets.length === 4, `expected 4 presets, got ${JSON.stringify(presets).slice(0, 200)}`);
  for (const [id, want] of Object.entries(expected)) {
    const got = presets.find((preset) => preset.id === id);
    check(got, `preset ${id} is missing`);
    check(got.role === want.role && got.authority === want.authority, `preset ${id}: role/authority should be ${want.role}/${want.authority}, got ${got.role}/${got.authority}`);
    const have = words(String(got.charter ?? ""));
    const need = [...words(want.charter)];
    const covered = need.filter((word) => have.has(word)).length / need.length;
    check(covered >= 0.8, `preset ${id}: charter covers only ${Math.round(covered * 100)}% of the Rust charter's words`);
  }
  const shown = bus.cli(null, ["preset", "show", "lead"]);
  check(shown.code === 0 && shown.stdout.includes(expected.lead.charter.split("\n").find((line) => line.trim()).trim()), "`preset show lead` must print the lead charter");
  const unknown = bus.cli(null, ["preset", "show", "no-such"]);
  check(unknown.code !== 0 && /no-such/.test(unknown.stderr), "an unknown preset must fail and name the id");
  const text = bus.cli(null, ["preset", "list"]);
  check(text.code === 0 && Object.keys(expected).every((id) => text.stdout.includes(id)), "the text form of `preset list` must name all four presets");
  check(sh("git", ["-C", CHECKOUT, "diff", "--quiet", BASE_SHA, "HEAD", "--", "rust"]).code === 0, "the task must not change rust/");
} finally {
  bus.cleanup();
}
