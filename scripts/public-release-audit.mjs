#!/usr/bin/env node

import { existsSync, readFileSync, statSync } from "node:fs";
import { basename, resolve } from "node:path";
import { execFileSync } from "node:child_process";

const ROOT = resolve(process.cwd());
const CHECK_HISTORY = process.argv.includes("--history");
const MAX_TEXT_BYTES = 5 * 1024 * 1024;

const knownPrivateMarkers = [
  "Novel" + " Biotech",
  ["MacBook", "Pro", "5376"].join("-"),
  "windsurf_" + "api_key",
];

const contentRules = [
  { name: "private macOS home path", regex: /\/Users\/[^/\s]+\//g },
  { name: "private Linux home path", regex: /\/home\/[^/\s]+\//g },
  { name: "private Windows home path", regex: /[A-Za-z]:\\Users\\[^\\\s]+\\/g },
  { name: "private project marker", regex: new RegExp(knownPrivateMarkers.map(escapeRegex).join("|"), "gi") },
  { name: "private key block", regex: /-----BEGIN [A-Z ]*PRIVATE KEY-----/g },
  { name: "GitHub token", regex: /gh[pousr]_[A-Za-z0-9_]{20,}/g },
  { name: "OpenAI or Anthropic key", regex: /sk-(?:ant-)?[A-Za-z0-9_-]{16,}/g },
  { name: "AWS access key", regex: /AKIA[0-9A-Z]{16}/g },
  { name: "Slack token", regex: /xox[baprs]-[A-Za-z0-9-]{10,}/g },
  { name: "JWT-like token", regex: /eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}/g },
];

const allowedEmailDomains = new Set([
  "example.com",
  "example.org",
  "localhost",
  "noreply.github.com",
  "users.noreply.github.com",
]);
const emailRegex = /[A-Z0-9._%+-]+@([A-Z0-9.-]+\.[A-Z]{2,}|localhost)/gi;

const findings = [];

function escapeRegex(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function git(args, options = {}) {
  return execFileSync("git", args, {
    cwd: ROOT,
    encoding: options.encoding ?? "utf8",
    input: options.input,
    maxBuffer: 128 * 1024 * 1024,
    stdio: [options.input === undefined ? "ignore" : "pipe", "pipe", "pipe"],
  });
}

function isForbiddenPath(path) {
  const name = basename(path);
  if (name === ".env.example") return false;
  return (
    /^\.env(?:\..+)?$/i.test(name)
    || /\.(?:pem|p12|pfx|key|token|sqlite|sqlite3|db)(?:-(?:wal|shm))?$/i.test(name)
    || /^id_(?:rsa|dsa|ecdsa|ed25519)(?:\.pub)?$/i.test(name)
  );
}

function addFinding(scope, location, rule) {
  findings.push({ scope, location, rule });
}

function lineNumber(text, index) {
  let line = 1;
  for (let offset = 0; offset < index; offset += 1) {
    if (text.charCodeAt(offset) === 10) line += 1;
  }
  return line;
}

function inspectText(path, text, scope = "working tree") {
  for (const { name, regex } of contentRules) {
    regex.lastIndex = 0;
    const match = regex.exec(text);
    if (match) addFinding(scope, `${path}:${lineNumber(text, match.index)}`, name);
  }

  emailRegex.lastIndex = 0;
  for (const match of text.matchAll(emailRegex)) {
    const domain = match[1].toLowerCase();
    if (!allowedEmailDomains.has(domain)) {
      addFinding(scope, `${path}:${lineNumber(text, match.index)}`, "non-public email address");
      break;
    }
  }
}

function workingTreeFiles() {
  const output = git(["ls-files", "-z", "--cached", "--others", "--exclude-standard"]);
  return output.split("\0").filter(Boolean).sort();
}

function inspectWorkingTree() {
  const files = workingTreeFiles();
  for (const path of files) {
    const absolute = resolve(ROOT, path);
    if (!existsSync(absolute)) continue;
    if (isForbiddenPath(path)) addFinding("working tree", path, "sensitive filename is tracked or publishable");

    const stat = statSync(absolute);
    if (!stat.isFile() || stat.size > MAX_TEXT_BYTES) continue;
    const buffer = readFileSync(absolute);
    if (buffer.includes(0)) continue;
    inspectText(path, buffer.toString("utf8"));
  }
  return files.length;
}

function inspectHistory() {
  const metadata = git(["log", "--all", "--format=%H%x09%ae%x09%ce"])
    .split("\n")
    .filter(Boolean);
  for (const row of metadata) {
    const [commit, authorEmail = "", committerEmail = ""] = row.split("\t");
    for (const [field, address] of [["author", authorEmail], ["committer", committerEmail]]) {
      const domain = address.split("@").pop()?.toLowerCase() ?? "";
      if (!allowedEmailDomains.has(domain)) {
        addFinding("history", commit.slice(0, 12), `non-public ${field} email`);
      }
    }
  }

  const trees = [...new Set(git(["log", "--all", "--format=%T"]).split("\n").filter(Boolean))];
  const historyPattern = [
    "/" + "Users/[^/]+/",
    "/" + "home/[^/]+/",
    "[A-Za-z]:\\\\Users\\\\[^\\\\]+\\\\",
    ...knownPrivateMarkers.map(escapeRegex),
    "-----BEGIN [A-Z ]*PRIVATE KEY-----",
    "gh[pousr]_[A-Za-z0-9_]{20,}",
    "sk-(ant-)?[A-Za-z0-9_-]{16,}",
    "AKIA[0-9A-Z]{16}",
    "xox[baprs]-[A-Za-z0-9-]{10,}",
  ].join("|");

  for (const tree of trees) {
    const paths = git(["ls-tree", "-r", "--name-only", tree]).split("\n").filter(Boolean);
    const sensitivePath = paths.find(isForbiddenPath);
    if (sensitivePath) addFinding("history", tree.slice(0, 12), "sensitive filename exists in a reachable revision");

    try {
      git(["grep", "-I", "-n", "-E", historyPattern, tree, "--"]);
      addFinding("history", tree.slice(0, 12), "private path, project marker, or credential pattern exists in a reachable revision");
    } catch (error) {
      if (error?.status !== 1) throw error;
    }
  }

  return { commits: metadata.length, trees: trees.length };
}

let fileCount = 0;
let historyCount = null;

try {
  fileCount = inspectWorkingTree();
  if (CHECK_HISTORY) historyCount = inspectHistory();
} catch (error) {
  process.stderr.write(`public-release-audit: could not complete: ${error.message}\n`);
  process.exit(2);
}

if (findings.length) {
  process.stderr.write(`public-release-audit: FAIL (${findings.length} finding${findings.length === 1 ? "" : "s"})\n`);
  for (const finding of findings.slice(0, 100)) {
    process.stderr.write(`- ${finding.scope}: ${finding.location}: ${finding.rule}\n`);
  }
  if (findings.length > 100) process.stderr.write(`- ${findings.length - 100} more findings omitted\n`);
  process.exit(1);
}

const historySummary = historyCount
  ? `; ${historyCount.commits} commits and ${historyCount.trees} unique trees`
  : "";
process.stdout.write(`public-release-audit: PASS (${fileCount} files${historySummary})\n`);
