// Sheets: NewTaskSheet + DecisionSheet — same fields, validation, copy and
// payloads as TaskSheets.swift.

import { useEffect, useRef, useState } from "preact/hooks";
import { store } from "../store";
import type { Id, TaskRecord } from "../model";

function useDismiss(onClose: () => void, disabled: boolean) {
  useEffect(() => {
    if (disabled) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [disabled, onClose]);
}

function Sheet(props: { width?: number; onClose: () => void; children: preact.ComponentChildren }) {
  const busy = store.busy.value;
  useDismiss(props.onClose, busy);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    ref.current?.querySelector<HTMLElement>("input, textarea, select, button")?.focus();
  }, []);
  return (
    <div
      class="sheet-scrim"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget && !busy) props.onClose();
      }}
    >
      <div class="sheet" role="dialog" aria-modal="true" style={{ width: props.width ?? 540 }} ref={ref}>
        {props.children}
      </div>
    </div>
  );
}

export function NewTaskSheet() {
  const [title, setTitle] = useState("");
  const [brief, setBrief] = useState("");
  const [acceptance, setAcceptance] = useState("");
  const [assignee, setAssignee] = useState("");
  const [reviewer, setReviewer] = useState("operator");
  const [scope, setScope] = useState("");
  const [priority, setPriority] = useState("normal");
  const [failure, setFailure] = useState<string | null>(null);
  const agents = store.agents.value;
  const busy = store.busy.value;

  const valid =
    title.trim() !== "" &&
    brief.trim() !== "" &&
    acceptance.trim() !== "" &&
    (assignee === "" || reviewer !== assignee) &&
    title.length <= 500;

  const close = () => (store.showNewTask.value = false);

  const create = async () => {
    const paths = scope
      .split(",")
      .map((s) => s.trim())
      .filter((s) => s !== "");
    const payload: Record<string, unknown> = {
      title,
      brief,
      acceptance,
      to: assignee === "" ? null : assignee,
      reviewer,
      priority,
      pathScopes: paths,
      project: store.project.value,
    };
    if (await store.mutate("createTask", payload)) close();
    else setFailure(store.error.value ?? "The task could not be created.");
  };

  return (
    <Sheet width={640} onClose={close}>
      <h2>Give your agents a clear task</h2>
      <p class="sheet-sub">Describe the outcome. Define what good looks like. Keep review independent.</p>
      <div class="form-grid">
        <span class="f-label">Task title</span>
        <div class="field">
          <input
            type="text"
            value={title}
            onInput={(e) => setTitle((e.target as HTMLInputElement).value)}
            placeholder="e.g. Add validation to the signup form"
            aria-label="Task title"
          />
        </div>
        <span class="f-label">Brief</span>
        <div class="field">
          <textarea
            value={brief}
            onInput={(e) => setBrief((e.target as HTMLTextAreaElement).value)}
            style={{ minHeight: 100 }}
            aria-label="Task brief"
          />
        </div>
        <span class="f-label">Acceptance criteria</span>
        <div class="field">
          <textarea
            value={acceptance}
            onInput={(e) => setAcceptance((e.target as HTMLTextAreaElement).value)}
            style={{ minHeight: 80 }}
            aria-label="Acceptance criteria"
          />
        </div>
        <span class="f-label">Worker</span>
        <div class="field">
          <select
            value={assignee}
            onChange={(e) => {
              const next = (e.target as HTMLSelectElement).value;
              setAssignee(next);
              if (reviewer === next) setReviewer("operator");
            }}
            aria-label="Worker"
          >
            <option value="">Any eligible worker</option>
            {agents.map((a) => (
              <option value={a.id}>{a.id}</option>
            ))}
          </select>
        </div>
        <span class="f-label">Reviewer</span>
        <div class="field">
          <select value={reviewer} onChange={(e) => setReviewer((e.target as HTMLSelectElement).value)} aria-label="Reviewer">
            <option value="operator">Me (operator)</option>
            {agents
              .filter((a) => a.id !== assignee)
              .map((a) => (
                <option value={a.id}>{a.id}</option>
              ))}
          </select>
        </div>
      </div>
      <details class="details">
        <summary>Scope and priority</summary>
        <div class="details-body">
          <div class="field">
            <span class="f-label">File scope</span>
            <input
              type="text"
              value={scope}
              onInput={(e) => setScope((e.target as HTMLInputElement).value)}
              placeholder="src/forms/, tests/ (comma separated)"
              aria-label="File scope"
            />
            <p class="inline-note">A scope is task guidance, not an operating-system sandbox.</p>
          </div>
          <div class="field">
            <span class="f-label">Priority</span>
            <select value={priority} onChange={(e) => setPriority((e.target as HTMLSelectElement).value)} aria-label="Priority">
              {["low", "normal", "high", "urgent"].map((p) => (
                <option value={p}>{p.charAt(0).toUpperCase() + p.slice(1)}</option>
              ))}
            </select>
          </div>
        </div>
      </details>
      {failure && <p class="form-error">{failure}</p>}
      <div class="actions">
        <p class="inline-note">Creating a task doesn’t start an agent.</p>
        <span class="spacer" />
        <button class="btn" onClick={close} disabled={busy}>
          Cancel
        </button>
        <button class="btn primary" onClick={() => void create()} disabled={!valid || !store.canWrite.value}>
          Create Task
        </button>
      </div>
    </Sheet>
  );
}

export type TaskDecision = "accept" | "changes" | "requeue" | "cancel";

const DECISION_COPY: Record<TaskDecision, { title: string; button: string; explanation: string }> = {
  accept: {
    title: "Accept this work?",
    button: "Accept Work",
    explanation: "Confirm that you reviewed the submission against its acceptance criteria. This marks the task accepted.",
  },
  changes: {
    title: "What needs to change?",
    button: "Request Changes",
    explanation: "Give actionable feedback. The worker can revise and submit again.",
  },
  requeue: {
    title: "Return this task to the queue?",
    button: "Return to Queue",
    explanation:
      "This releases the current claim and preserves the task’s requirements. A running provider may still be working; stop it first if needed.",
  },
  cancel: {
    title: "Cancel this task?",
    button: "Cancel Task",
    explanation: "This closes the task but preserves its history. It does not terminate an already running provider.",
  },
};

export function DecisionSheet(props: { task: TaskRecord | { id: Id; title: string }; decision: TaskDecision; onClose: () => void }) {
  const [feedback, setFeedback] = useState("");
  const [failure, setFailure] = useState<string | null>(null);
  const busy = store.busy.value;
  const copy = DECISION_COPY[props.decision];

  const submit = async () => {
    const action =
      props.decision === "accept" || props.decision === "changes" ? "reviewTask" : props.decision;
    const payload: Record<string, unknown> = { id: props.task.id };
    if (action === "reviewTask") {
      payload.accept = props.decision === "accept";
      payload.feedback = feedback;
    } else {
      payload.reason = feedback;
    }
    if (await store.mutate(action, payload)) props.onClose();
    else setFailure(store.error.value ?? "The task could not be updated.");
  };

  return (
    <Sheet width={540} onClose={props.onClose}>
      <h2>{copy.title}</h2>
      <p style={{ fontWeight: 650, margin: 0 }}>
        #{String(props.task.id)} · {props.task.title}
      </p>
      <p class="sheet-sub">{copy.explanation}</p>
      <div class="field">
        <span class="f-label">{props.decision === "accept" ? "Review note" : "Feedback / reason"}</span>
        <textarea
          value={feedback}
          onInput={(e) => setFeedback((e.target as HTMLTextAreaElement).value)}
          style={{ minHeight: 130 }}
          aria-label="Feedback or reason"
        />
      </div>
      {failure && <p class="form-error">{failure}</p>}
      <div class="actions">
        <span class="spacer" />
        <button class="btn" onClick={props.onClose} disabled={busy}>
          Go Back
        </button>
        <button
          class={`btn primary ${props.decision === "cancel" ? "danger" : ""}`}
          onClick={() => void submit()}
          disabled={!store.canWrite.value || feedback.trim() === ""}
        >
          {copy.button}
        </button>
      </div>
    </Sheet>
  );
}
