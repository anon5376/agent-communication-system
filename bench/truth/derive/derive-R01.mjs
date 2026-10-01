// derive-R01.mjs — which declared config fields are never read outside validateConfig?
// Usage: node derive-R01.mjs <repo-root> [--verbose]
//   <repo-root> must be a checkout of commit 4d4cf5a9f4ca0e819dfc9b97d6610cc05fbf1893 with
//   node_modules present (or symlinked) so that `typescript`, `zod`, `@types/node` resolve.
// Method: TypeScript LanguageService.findReferences on each property declaration of the
// interfaces below (type-aware: follows symbol identity, including inherited props via
// ResolvedAgent extends AgentDefinition and Partial<...> mapped types). A reference counts as
// a READ iff: it is under src/, not the declaration, not isWriteAccess (assignments and
// object-literal property assignments are writes), and not inside validateConfig's body.
import { createRequire } from "node:module";
import { readFileSync, existsSync } from "node:fs";
import { join, resolve, relative } from "node:path";

const root = resolve(process.argv[2] ?? ".");
const verbose = process.argv.includes("--verbose");
const require = createRequire(join(root, "package.json"));
const ts = require("typescript");

const INTERFACES = ["BusConstraints", "AgentPermissions", "AgentDefinition", "RolePolicy", "HarnessFeatureSet"];
const configPath = join(root, "src/config.ts");

const tsconfig = ts.getParsedCommandLineOfConfigFile(join(root, "tsconfig.json"), {}, {
  ...ts.sys, onUnRecoverableConfigFileDiagnostic: (d) => { throw new Error(ts.flattenDiagnosticMessageText(d.messageText, "\n")); },
});
const files = tsconfig.fileNames;
const host = {
  getScriptFileNames: () => files,
  getScriptVersion: () => "1",
  getScriptSnapshot: (f) => existsSync(f) ? ts.ScriptSnapshot.fromString(readFileSync(f, "utf8")) : undefined,
  getCurrentDirectory: () => root,
  getCompilationSettings: () => tsconfig.options,
  getDefaultLibFileName: (o) => ts.getDefaultLibFilePath(o),
  fileExists: ts.sys.fileExists, readFile: ts.sys.readFile, readDirectory: ts.sys.readDirectory,
  directoryExists: ts.sys.directoryExists, getDirectories: ts.sys.getDirectories,
};
const ls = ts.createLanguageService(host, ts.createDocumentRegistry());
const program = ls.getProgram();
const sf = program.getSourceFile(configPath);

let vcStart = -1, vcEnd = -1;
const fields = [];
ts.forEachChild(sf, (node) => {
  if (ts.isFunctionDeclaration(node) && node.name?.text === "validateConfig") {
    vcStart = node.body.getStart(sf); vcEnd = node.body.getEnd();
  }
  if (ts.isInterfaceDeclaration(node) && INTERFACES.includes(node.name.text)) {
    for (const m of node.members) {
      if (ts.isPropertySignature(m)) fields.push({ iface: node.name.text, name: m.name.getText(sf), pos: m.name.getStart(sf), line: sf.getLineAndCharacterOfPosition(m.name.getStart(sf)).line + 1 });
    }
  }
});
if (vcStart < 0) throw new Error("validateConfig not found");

const unread = [];
for (const f of fields) {
  const refs = (ls.findReferences(configPath, f.pos) ?? []).flatMap((s) => s.references);
  const seen = new Set();
  const reads = [], writes = [], inValidate = [];
  for (const r of refs) {
    const key = `${r.fileName}:${r.textSpan.start}`;
    if (seen.has(key)) continue; seen.add(key);
    const rel = relative(root, r.fileName);
    if (!rel.startsWith("src/")) continue;
    const rsf = program.getSourceFile(r.fileName);
    const loc = `${rel}:${rsf.getLineAndCharacterOfPosition(r.textSpan.start).line + 1}`;
    if (r.isDefinition && r.fileName === configPath && r.textSpan.start === f.pos) continue;
    if (r.isWriteAccess) { writes.push(loc); continue; }
    if (r.fileName === configPath && r.textSpan.start >= vcStart && r.textSpan.start < vcEnd) { inValidate.push(loc); continue; }
    reads.push(loc);
  }
  const id = `${f.iface}.${f.name}`;
  if (verbose) console.log(`${id.padEnd(42)} reads=${reads.length} writes=${writes.length} inValidate=${inValidate.length}  ${reads.slice(0, 4).join(" ")}${reads.length > 4 ? " ..." : ""}`);
  if (reads.length === 0) unread.push({ id, decl: `src/config.ts:${f.line}`, writes, inValidate });
}
console.log(`\nUNREAD outside validateConfig (${unread.length}):`);
for (const u of unread) console.log(`${u.id}\t${u.decl}\twrites=[${u.writes.join(", ")}]\tinValidate=[${u.inValidate.join(", ")}]`);
