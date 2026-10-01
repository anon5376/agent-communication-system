// I11: `qagent export --format html --out FILE` writes a self-contained report of the whole bus.
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { check, newBus } from "./lib.mjs";

const bus = newBus();
try {
  bus.json(null, ["agent", "add", "lead", "--role", "manager", "--authority", "manager"]);
  bus.json(null, ["agent", "add", "worker", "--role", "worker"]);
  const titles = ["Parse the <config> file", "Wire the \"cache\" & retry", "Third task"];
  const ids = titles.map((title) => bus.json("lead", ["task", "add", title, "--to", "worker"]).id);
  bus.json("worker", ["task", "claim", String(ids[0])]);
  bus.json("worker", ["task", "note", String(ids[0]), "skeleton done"]);
  bus.json("worker", ["task", "submit", String(ids[0]), "--summary", "parser done"]);
  bus.json("lead", ["task", "review", String(ids[0]), "--accept", "--feedback", "good"]);
  bus.json("lead", ["send", "worker", "ping", "hello there"]);

  const out = join(bus.home, "export.html");
  const result = bus.cli(null, ["export", "--format", "html", "--out", out]);
  check(result.code === 0, `export failed: ${result.stderr.trim() || result.stdout.trim()}`);
  check(existsSync(out), "export did not write the --out file");
  const html = readFileSync(out, "utf8");
  check(/<html/i.test(html), "the output is not an HTML document");
  for (const id of ids) check(html.includes(`#${id}`), `task id #${id} is not in the export`);
  check(html.includes("Parse the &lt;config&gt; file"), "task titles must be present and HTML-escaped");
  check(!html.includes("Parse the <config> file"), "a title was written unescaped");
  for (const word of ["skeleton done", "parser done", "hello there", "worker", "lead"]) check(html.includes(word), `the export lacks "${word}" (notes, results, messages and agents belong in it)`);
  check(!/https?:\/\//i.test(html), "the export contains an external URL");
  check(!/<script[^>]+src=|<link[^>]+href=|@import|url\(/i.test(html), "the export loads an external resource");
} finally {
  bus.cleanup();
}
