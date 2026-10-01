// Task files: `bench/tasks/<ID>-<slug>.md` with a small front matter block, then `## Brief` and `## Acceptance`.
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { BenchError } from "./util.mjs";

const ID = /^[IR]\d{2}$/;

function scalar(raw) {
  const value = raw.trim();
  if (value === "true") return true;
  if (value === "false") return false;
  if (value.startsWith("[") && value.endsWith("]")) {
    return value.slice(1, -1).split(",").map((item) => item.trim()).filter(Boolean);
  }
  return value.replace(/^["']|["']$/g, "");
}

export function parseTask(text, file = "task") {
  const match = text.match(/^---\n([\s\S]*?)\n---\n([\s\S]*)$/);
  if (!match) throw new BenchError(`${file}: missing front matter`);
  const meta = {};
  for (const line of match[1].split("\n")) {
    if (!line.trim() || line.trim().startsWith("#")) continue;
    const colon = line.indexOf(":");
    if (colon < 0) throw new BenchError(`${file}: bad front matter line "${line}"`);
    meta[line.slice(0, colon).trim()] = scalar(line.slice(colon + 1));
  }
  const sections = {};
  let current = null;
  for (const line of match[2].split("\n")) {
    const heading = line.match(/^## (.+)$/);
    if (heading) { current = heading[1].trim().toLowerCase(); sections[current] = []; } else if (current) sections[current].push(line);
  }
  const body = (name) => (sections[name] ?? []).join("\n").trim();
  const task = {
    id: String(meta.id ?? ""),
    kind: String(meta.kind ?? ""),
    role: String(meta.role ?? ""),
    title: String(meta.title ?? ""),
    mini: meta.mini === true,
    depends: Array.isArray(meta.depends) ? meta.depends : [],
    scope: Array.isArray(meta.scope) ? meta.scope : [],
    validator: meta.validator ? String(meta.validator) : null,
    truth: meta.truth ? String(meta.truth) : null,
    brief: body("brief"),
    acceptance: body("acceptance"),
  };
  if (!ID.test(task.id)) throw new BenchError(`${file}: id must look like I01 or R01`);
  if (!["implementation", "research"].includes(task.kind)) throw new BenchError(`${file}: kind must be implementation or research`);
  if ((task.kind === "implementation") !== task.id.startsWith("I")) throw new BenchError(`${file}: id ${task.id} does not match kind ${task.kind}`);
  if (!task.role || !task.title || !task.brief || !task.acceptance) throw new BenchError(`${file}: role, title, Brief and Acceptance are required`);
  if (task.kind === "implementation" && !task.validator) throw new BenchError(`${file}: implementation tasks need a validator`);
  if (task.kind === "research" && !task.truth) throw new BenchError(`${file}: research tasks need a truth file`);
  return task;
}

export function loadTasks(dir) {
  const files = readdirSync(dir).filter((name) => /^[IR]\d{2}-.*\.md$/.test(name)).sort();
  const tasks = files.map((name) => parseTask(readFileSync(join(dir, name), "utf8"), name));
  const ids = new Set();
  for (const task of tasks) {
    if (ids.has(task.id)) throw new BenchError(`duplicate task id ${task.id}`);
    ids.add(task.id);
  }
  for (const task of tasks) {
    for (const dep of task.depends) if (!ids.has(dep)) throw new BenchError(`${task.id} depends on unknown task ${dep}`);
  }
  return tasks.sort((a, b) => a.id.localeCompare(b.id));
}

/** Task key from a bus task title ("I03: ..."), or null. */
export function taskKey(title) {
  return String(title ?? "").match(/^([IR]\d{2}):/)?.[1] ?? null;
}
