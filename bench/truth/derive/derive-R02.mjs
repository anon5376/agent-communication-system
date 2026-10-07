// derive-R02.mjs — which functions execute SQL against the bus `agents` table?
// Usage: node derive-R02.mjs <ts-repo-root> <rust-repo-root>
//   <ts-repo-root>: checkout of 4d4cf5a9f4ca0e819dfc9b97d6610cc05fbf1893 (needs node_modules/typescript,
//                   or a node_modules symlink); scans src/**/*.ts
//   <rust-repo-root>: worktree of origin/rust-port (c6df26b3ec0235e38404b33b382c1fa824ba12a5); scans rust/src/**/*.rs
// Every string/template literal whose text references the table `agents` in SQL position
// (FROM/JOIN/INTO/UPDATE/TABLE/REFERENCES agents) is mapped to its enclosing named function.
// Output: one line per hit (kind = SCHEMA / READ / WRITE guess), then the distinct function list.
// Hand verification is still required (e.g. reads of OTHER databases' `agents` tables in import code).
import { createRequire } from "node:module";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, resolve, relative } from "node:path";

const tsRoot = resolve(process.argv[2] ?? ".");
const rsRoot = resolve(process.argv[3] ?? ".");
const ts = createRequire(join(tsRoot, "package.json"))("typescript");

const SQL_RE = /\b(FROM|JOIN|INTO|UPDATE|TABLE(?:\s+IF\s+NOT\s+EXISTS)?|REFERENCES)\s+agents\b/i;
const classify = (s) => /CREATE\s+TABLE/i.test(s) ? "SCHEMA"
  : /\b(INSERT\s+(OR\s+\w+\s+)?INTO\s+agents|UPDATE\s+agents|DELETE\s+FROM\s+agents)\b/i.test(s) ? "WRITE" : "READ";

function walk(dir, ext, out = []) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) walk(p, ext, out); else if (p.endsWith(ext)) out.push(p);
  }
  return out.sort();
}

const hits = [];

// ---------- TypeScript ----------
function fnName(node) {
  for (let n = node.parent; n; n = n.parent) {
    if ((ts.isFunctionDeclaration(n) || ts.isMethodDeclaration(n) || ts.isGetAccessor(n)) && n.name) return n.name.getText();
    if (ts.isConstructorDeclaration(n)) return "constructor";
  }
  for (let n = node.parent; n; n = n.parent) {
    if ((ts.isArrowFunction(n) || ts.isFunctionExpression(n)) && n.parent && ts.isVariableDeclaration(n.parent)) return n.parent.name.getText();
  }
  return "<module>";
}
for (const file of walk(join(tsRoot, "src"), ".ts")) {
  const text = readFileSync(file, "utf8");
  const sf = ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true);
  const visit = (node) => {
    let lit = null;
    if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) lit = node.text;
    else if (ts.isTemplateExpression(node)) lit = node.head.text + node.templateSpans.map((s) => "${}" + s.literal.text).join("");
    if (lit !== null && SQL_RE.test(lit)) {
      hits.push({ lang: "ts", file: relative(tsRoot, file), line: sf.getLineAndCharacterOfPosition(node.getStart(sf)).line + 1, fn: fnName(node), kind: classify(lit), sql: lit.replace(/\s+/g, " ").trim() });
    }
    if (!ts.isTemplateExpression(node)) ts.forEachChild(node, visit); else node.templateSpans.forEach((s) => visit(s.expression));
  };
  visit(sf);
}

// ---------- Rust (small lexer: comments, strings, raw strings, char literals, fn/brace tracking) ----------
function scanRust(src) {
  const out = [];
  const stack = []; // {name, depth}
  let depth = 0, pending = null, line = 1, i = 0;
  const n = src.length;
  while (i < n) {
    const c = src[i];
    if (c === "\n") { line++; i++; continue; }
    if (c === "/" && src[i + 1] === "/") { while (i < n && src[i] !== "\n") i++; continue; }
    if (c === "/" && src[i + 1] === "*") { let d = 1; i += 2; while (i < n && d) { if (src[i] === "\n") line++; if (src[i] === "/" && src[i + 1] === "*") { d++; i++; } else if (src[i] === "*" && src[i + 1] === "/") { d--; i++; } i++; } continue; }
    const raw = /^b?r(#*)"/.exec(src.slice(i, i + 12));
    if (raw && !/[A-Za-z0-9_]/.test(src[i - 1] ?? "")) {
      const close = '"' + raw[1]; const start = i + raw[0].length; const end = src.indexOf(close, start);
      const s = src.slice(start, end); out.push({ line, fn: stack.at(-1)?.name ?? "<module>", s });
      line += (s.match(/\n/g) ?? []).length; i = end + close.length; continue;
    }
    if (c === '"' || (c === "b" && src[i + 1] === '"' && !/[A-Za-z0-9_]/.test(src[i - 1] ?? ""))) {
      const startLine = line; i += c === "b" ? 2 : 1; let s = "";
      while (i < n && src[i] !== '"') { if (src[i] === "\\") { if (src[i + 1] === "\n") line++; s += src[i] + src[i + 1]; i += 2; continue; } if (src[i] === "\n") line++; s += src[i++]; }
      i++; out.push({ line: startLine, fn: stack.at(-1)?.name ?? "<module>", s }); continue;
    }
    if (c === "'") { const m = /^'(\\.[^']*|[^\\'])'/.exec(src.slice(i, i + 12)); if (m) { i += m[0].length; continue; } i++; continue; }
    if (/[A-Za-z_]/.test(c)) {
      const m = /^[A-Za-z_][A-Za-z0-9_]*/.exec(src.slice(i, i + 80))[0];
      if (m === "fn" && !/[A-Za-z0-9_]/.test(src[i - 1] ?? "")) { const nm = /^fn\s+([A-Za-z_][A-Za-z0-9_]*)/.exec(src.slice(i, i + 100)); if (nm) pending = nm[1]; }
      i += m.length; continue;
    }
    if (c === "{") { depth++; if (pending) { stack.push({ name: pending, depth }); pending = null; } }
    else if (c === "}") { if (stack.length && stack.at(-1).depth === depth) stack.pop(); depth--; }
    else if (c === ";") pending = null;
    i++;
  }
  return out;
}
for (const file of walk(join(rsRoot, "rust/src"), ".rs")) {
  for (const lit of scanRust(readFileSync(file, "utf8"))) {
    if (SQL_RE.test(lit.s)) hits.push({ lang: "rs", file: relative(rsRoot, file), line: lit.line, fn: lit.fn, kind: classify(lit.s), sql: lit.s.replace(/\\\n\s*/g, "").replace(/\s+/g, " ").trim() });
  }
}

for (const h of hits) console.log(`${h.kind.padEnd(6)} ${h.file}:${h.line}\t${h.fn}\t${h.sql.slice(0, 110)}`);
const fns = new Map();
for (const h of hits) { if (h.kind === "SCHEMA") continue; const id = `${h.file}:${h.fn}`; const e = fns.get(id) ?? new Set(); e.add(h.kind); fns.set(id, e); }
console.log(`\nFUNCTIONS (${fns.size}, schema excluded, before hand verification):`);
for (const [id, k] of fns) console.log(`${id}\t${[...k].join("+")}`);
