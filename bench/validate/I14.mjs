// I14: GET /api/tasks/tree returns the task tree by parent_id; a test covers it.
import { readdirSync, readFileSync } from "node:fs";
import { request } from "node:http";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { CHECKOUT, check, newBus } from "./lib.mjs";

const tests = readdirSync(join(CHECKOUT, "tests")).filter((name) => name.endsWith(".test.ts") && readFileSync(join(CHECKOUT, "tests", name), "utf8").includes("/api/tasks/tree"));
check(tests.length > 0, "no test under tests/ exercises /api/tasks/tree");

const get = (port, path, cookie) => new Promise((resolve, reject) => {
  const req = request({ host: "127.0.0.1", port, path, headers: { host: `127.0.0.1:${port}`, ...(cookie ? { cookie } : {}) } }, (res) => {
    let body = "";
    res.setEncoding("utf8").on("data", (chunk) => { body += chunk; }).on("end", () => resolve({ status: res.statusCode, body }));
  });
  req.on("error", reject).end();
});

const bus = newBus();
let dashboard;
try {
  bus.json(null, ["agent", "add", "lead", "--role", "manager", "--authority", "manager"]);
  bus.json(null, ["agent", "add", "worker", "--role", "worker"]);
  const root = bus.json("lead", ["task", "add", "Root", "--to", "worker"]);
  const child = bus.json("lead", ["task", "add", "Child", "--parent", String(root.id)]);
  const grandchild = bus.json("lead", ["task", "add", "Grandchild", "--parent", String(child.id), "--to", "worker"]);
  const lone = bus.json("lead", ["task", "add", "Lone root"]);
  bus.json("worker", ["task", "claim", String(root.id)]);

  const { startDashboard } = await import(pathToFileURL(join(CHECKOUT, "dist", "dashboard", "server.js")).href);
  dashboard = await startDashboard({ dbPath: bus.dbPath, port: 0, safetyMs: 10_000 });
  const unauthenticated = await get(dashboard.port, "/api/tasks/tree");
  check(unauthenticated.status === 401 || unauthenticated.status === 403, `the tree endpoint must require a session, got ${unauthenticated.status}`);
  const ticket = dashboard.signInUrl().split("#t=")[1];
  const session = await new Promise((resolve, reject) => {
    const payload = JSON.stringify({ ticket });
    const req = request({ host: "127.0.0.1", port: dashboard.port, method: "POST", path: "/session", headers: { host: `127.0.0.1:${dashboard.port}`, origin: `http://127.0.0.1:${dashboard.port}`, "content-type": "application/json" } }, (res) => {
      res.resume().on("end", () => resolve(String(res.headers["set-cookie"]?.[0] ?? "").split(";")[0]));
    });
    req.on("error", reject).end(payload);
  });
  const reply = await get(dashboard.port, "/api/tasks/tree", session);
  check(reply.status === 200, `GET /api/tasks/tree returned ${reply.status}`);
  const tree = JSON.parse(reply.body);
  check(Array.isArray(tree.roots), "the body needs a top-level `roots` array");
  const flatten = (nodes) => nodes.flatMap((node) => [node, ...flatten(node.children ?? [])]);
  const find = (nodes, id) => flatten(nodes).find((node) => node.id === id);
  check(tree.roots.map((node) => node.id).sort().join() === [root.id, lone.id].sort().join(), `roots should be #${root.id} and #${lone.id}, got ${tree.roots.map((node) => node.id)}`);
  const rootNode = find(tree.roots, root.id);
  check(rootNode.state === "claimed" && rootNode.assignee === "worker" && rootNode.title === "Root", `root node is wrong: ${JSON.stringify(rootNode)}`);
  check(rootNode.children.map((node) => node.id).join() === String(child.id), "the child must be listed under the root");
  check(find(rootNode.children, child.id).children.map((node) => node.id).join() === String(grandchild.id), "the grandchild must be nested under the child");
  check(find(tree.roots, grandchild.id).state === "open", "node states must be the task states");
} finally {
  await dashboard?.close();
  bus.cleanup();
}
