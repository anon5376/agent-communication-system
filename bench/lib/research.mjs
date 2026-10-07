// Scoring for research tasks: a submitted report is JSON in the task result's `details`:
//   { "items": ["..."], "claims": [{ "text": "...", "evidence": ["src/x.ts:12", "https://..."] }], "unresolved": ["..."] }
// `items` is the answer set compared with bench/truth/<ID>.json; claims are scored on their evidence.

function firstJson(text) {
  if (typeof text !== "string") return null;
  const start = text.indexOf("{");
  if (start < 0) return null;
  for (let end = text.lastIndexOf("}"); end > start; end = text.lastIndexOf("}", end - 1)) {
    try {
      const value = JSON.parse(text.slice(start, end + 1));
      if (value && typeof value === "object" && !Array.isArray(value)) return value;
    } catch { /* try a shorter span */ }
  }
  return null;
}

export function extractReport(result) {
  return firstJson(result?.details) ?? firstJson(result?.summary);
}

export function normalize(value) {
  return String(value).toLowerCase().replace(/[`'"]/g, "").replace(/\s+/g, " ").trim();
}

function evidenceList(claim) {
  const raw = claim?.evidence;
  return (Array.isArray(raw) ? raw : raw ? [raw] : []).map(String).filter(Boolean);
}

/** `path:12` or `path:12-20` -> { path, line }; http(s) -> url; anything else -> other. */
export function classifyEvidence(entry) {
  if (/^https?:\/\/\S+$/.test(entry)) return { kind: "url" };
  const match = entry.trim().match(/^`?(?:\.\/)?([\w./@-]+\.[A-Za-z0-9]+):(\d+)(?:-\d+)?`?$/);
  return match ? { kind: "file", path: match[1], line: Number(match[2]) } : { kind: "other" };
}

/**
 * @param report parsed report or null
 * @param truth { items: [{ id, aliases? }] }
 * @param readFile (path) => file text at the base commit, or null
 */
export function scoreResearch(report, truth, readFile) {
  if (!report) return { reportFound: false, claims: 0, claimsWithEvidence: 0, verifiedEvidence: 0, evidenceRate: null, precision: null, recall: null, reported: 0, unresolved: 0 };
  const claims = Array.isArray(report.claims) ? report.claims : [];
  let withEvidence = 0;
  let verified = 0;
  for (const claim of claims) {
    const entries = evidenceList(claim);
    if (entries.length) withEvidence += 1;
    const ok = entries.some((entry) => {
      const parsed = classifyEvidence(entry);
      if (parsed.kind !== "file") return false;
      const text = readFile(parsed.path);
      return text !== null && parsed.line >= 1 && parsed.line <= text.split("\n").length;
    });
    if (ok) verified += 1;
  }
  const reported = new Set((Array.isArray(report.items) ? report.items : []).map(normalize).filter(Boolean));
  const truthItems = truth.items.map((item) => new Set([item.id, ...(item.aliases ?? [])].map(normalize)));
  const truthKeys = new Set(truthItems.flatMap((keys) => [...keys]));
  const matchedTruth = truthItems.filter((keys) => [...keys].some((key) => reported.has(key))).length;
  const matchedReported = [...reported].filter((item) => truthKeys.has(item)).length;
  return {
    reportFound: true,
    claims: claims.length,
    claimsWithEvidence: withEvidence,
    verifiedEvidence: verified,
    evidenceRate: claims.length ? verified / claims.length : null,
    precision: reported.size ? matchedReported / reported.size : null,
    recall: truthItems.length ? matchedTruth / truthItems.length : null,
    reported: reported.size,
    unresolved: Array.isArray(report.unresolved) ? report.unresolved.length : 0,
  };
}
