import { describe, expect, it } from "vitest";
import { LosslessNumber, parse as parseLossless } from "lossless-json";
import {
  filterTasks,
  isClosed,
  parseSnapshot,
  parseTask,
  parseTaskDetail,
  stateTitle,
  taskExplanation,
  type TaskRecord,
} from "./model";

const BIG = "9007199254740993"; // 2^53 + 1, beyond JS safe integers

function task(over: Partial<TaskRecord> = {}): TaskRecord {
  return {
    id: new LosslessNumber("1"),
    title: "t",
    brief: "",
    acceptance: "",
    state: "open",
    priority: "normal",
    pathScopes: [],
    dependencies: [],
    updatedMs: new LosslessNumber("1000"),
    createdMs: new LosslessNumber("1000"),
    ...over,
  };
}

describe("state mapping", () => {
  it("maps bus states to plain language, matching the macOS app", () => {
    expect(stateTitle("open")).toBe("Queued");
    expect(stateTitle("claimed")).toBe("Claimed");
    expect(stateTitle("submitted")).toBe("Needs review");
    expect(stateTitle("changes_requested")).toBe("Changes requested");
    expect(stateTitle("blocked")).toBe("Blocked");
    expect(stateTitle("accepted")).toBe("Accepted");
    expect(stateTitle("cancelled")).toBe("Cancelled");
    expect(stateTitle("failed")).toBe("Failed");
    expect(stateTitle("weird_state")).toBe("Weird state");
  });

  it("marks only accepted/failed/cancelled as closed", () => {
    for (const s of ["open", "claimed", "submitted", "changes_requested", "blocked"]) {
      expect(isClosed(s)).toBe(false);
    }
    for (const s of ["accepted", "failed", "cancelled"]) {
      expect(isClosed(s)).toBe(true);
    }
  });
});

describe("parsing", () => {
  it("keeps i64 ids exact beyond 2^53", () => {
    const record = parseTask(parseLossless(`{"id": ${BIG}, "title": "x", "state": "open", "updatedMs": 5}`));
    expect(String(record.id)).toBe(BIG);
  });

  it("requires the security-signalling snapshot fields", () => {
    const good = parseLossless(
      `{"dbPath":"d","simulated":true,"canOperate":true,"tasks":[],"agents":[],"messages":[],"truncated":false}`,
    );
    expect(parseSnapshot(good).simulated).toBe(true);
    const missing = parseLossless(`{"dbPath":"d","tasks":[]}`);
    expect(() => parseSnapshot(missing)).toThrow();
  });

  it("flattens task detail like the Rust bridge does", () => {
    const detail = parseTaskDetail(
      parseLossless(
        `{"id":7,"title":"t","state":"submitted","updatedMs":1,"notes":[{"id":1,"author":"a","tsMs":2,"body":"n"}],"messages":[{"seq":3,"tsMs":4,"sender":"s","subject":"u","body":"b"}]}`,
      ),
    );
    expect(String(detail.id)).toBe("7");
    expect(detail.notes[0]?.body).toBe("n");
    expect(detail.messages[0]?.subject).toBe("u");
  });
});

describe("filterTasks", () => {
  const tasks = [
    task({ id: new LosslessNumber("1"), state: "submitted", updatedMs: new LosslessNumber("10") }),
    task({ id: new LosslessNumber("2"), state: "open", updatedMs: new LosslessNumber("30") }),
    task({ id: new LosslessNumber("3"), state: "accepted", updatedMs: new LosslessNumber("20") }),
    task({ id: new LosslessNumber("4"), state: "claimed", title: "token cap", assignee: "impl-a", updatedMs: new LosslessNumber("40") }),
  ];

  it("hides closed tasks unless asked, submitted first", () => {
    const out = filterTasks(tasks, { reviewOnly: false, includeClosed: false, query: "" });
    expect(out.map((t) => String(t.id))).toEqual(["1", "4", "2"]);
  });

  it("reviewOnly shows only submitted", () => {
    const out = filterTasks(tasks, { reviewOnly: true, includeClosed: false, query: "" });
    expect(out.map((t) => String(t.id))).toEqual(["1"]);
  });

  it("matches title, id and assignee case-insensitively", () => {
    const byTitle = filterTasks(tasks, { reviewOnly: false, includeClosed: true, query: "TOKEN" });
    expect(byTitle.map((t) => String(t.id))).toEqual(["4"]);
    const byAssignee = filterTasks(tasks, { reviewOnly: false, includeClosed: true, query: "IMPL-A" });
    expect(byAssignee.map((t) => String(t.id))).toEqual(["4"]);
    const byId = filterTasks(tasks, { reviewOnly: false, includeClosed: true, query: "3" });
    expect(byId.map((t) => String(t.id))).toEqual(["3"]);
  });
});

describe("taskExplanation", () => {
  it("names dependencies for blocked tasks", () => {
    const t = task({ state: "blocked", dependencies: [new LosslessNumber("3"), new LosslessNumber("7")] });
    expect(taskExplanation(t)).toBe("Waiting on dependencies: #3, #7.");
  });

  it("explains submitted work waits on the reviewer", () => {
    const t = task({ state: "submitted", reviewer: "rev-1" });
    expect(taskExplanation(t)).toContain("rev-1");
  });
});

import { groupTasks, parseSubjectTag, relativeTime, taskGroup } from "./model";

describe("task grouping and message tags", () => {
  it("groups states into the list sections", () => {
    expect(taskGroup("submitted")).toBe("review");
    expect(taskGroup("failed")).toBe("stuck");
    expect(taskGroup("changes_requested")).toBe("active");
    expect(taskGroup("open")).toBe("queued");
    expect(taskGroup("accepted")).toBe("done");
    const groups = groupTasks([{ state: "open" }, { state: "submitted" }] as never);
    expect(groups.map((g) => g.group.id)).toEqual(["review", "queued"]);
  });
  it("parses bus subject tags", () => {
    expect(parseSubjectTag("[DONE #7 r1] hard usd cap")).toMatchObject({ label: "Submitted", taskId: "7", round: 1, rest: "hard usd cap" });
    expect(parseSubjectTag("[ACCEPTED #12] x")).toMatchObject({ label: "Accepted", taskId: "12", round: null });
    expect(parseSubjectTag("plain subject")).toBeNull();
  });
  it("formats relative time", () => {
    expect(relativeTime(1_000_000, 1_000_000 + 30_000)).toBe("just now");
    expect(relativeTime(0, 5 * 60_000)).toBe("5 min ago");
    expect(relativeTime(0, 3 * 3_600_000)).toBe("3 h ago");
  });
});

describe("orchestration", () => {
  it("expands a mission like crew::expand", async () => {
    const { expandMission, parseOrchestration } = await import("./model");
    const orch = parseOrchestration({
      simulated: false,
      configured: false,
      missions: [{ name: "fix", summary: "s", brief: "Fix {goal} now", acceptance: "- {goal} works", text: "", custom: false }],
      goals: [{ id: 7, title: "t", state: "open", updatedMs: 1 }],
    });
    const plan = expandMission(orch.missions[0]!, "login");
    expect(plan).toEqual({ title: "fix login", brief: "Fix login now", acceptance: "- login works" });
    const long = expandMission({ ...orch.missions[0]!, name: "run" }, "x".repeat(130));
    expect([...long.title].length).toBe(120);
    expect(orch.roles).toEqual([]);
  });
});
