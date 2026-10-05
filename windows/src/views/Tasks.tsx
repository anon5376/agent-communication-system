// TaskBrowser + TaskInspector — the split task list and detail pane,
// matching TaskBrowser.swift's filtering, sorting and sections.

import { useMemo, useState } from "preact/hooks";
import { EmptyState } from "../App";
import { Icon } from "../icons";
import {
  filterTasks,
  idEq,
  isClosed,
  stateIcon,
  stateTitle,
  stateTone,
  taskExplanation,
  timestamp,
  type TaskDetail,
  type TaskRecord,
} from "../model";
import { store } from "../store";
import { DecisionSheet, type TaskDecision } from "./Sheets";

export function TaskBrowser(props: { reviewOnly: boolean }) {
  const [query, setQuery] = useState("");
  const [includeClosed, setIncludeClosed] = useState(false);
  const tasks = store.tasks.value;
  const snap = store.snapshot.value;

  const filtered = useMemo(
    () => filterTasks(tasks, { reviewOnly: props.reviewOnly, includeClosed, query }),
    [tasks, props.reviewOnly, includeClosed, query],
  );

  return (
    <div class="task-split">
      <div class="task-pane">
        <div class="pane-head">
          <div class="pane-title">
            <span>{props.reviewOnly ? "Needs review" : "Tasks"}</span>
            <span class="count">{filtered.length}</span>
          </div>
          <input
            class="search-box"
            type="search"
            placeholder="Search tasks"
            value={query}
            onInput={(e) => setQuery((e.target as HTMLInputElement).value)}
            aria-label="Search tasks"
          />
          {!props.reviewOnly && (
            <label class="checkbox-row">
              <input
                type="checkbox"
                checked={includeClosed}
                onChange={(e) => setIncludeClosed((e.target as HTMLInputElement).checked)}
              />
              Show completed tasks
            </label>
          )}
        </div>
        {filtered.length === 0 ? (
          <EmptyState
            icon={props.reviewOnly ? "tray" : "checklist"}
            title={query.trim() === "" ? (props.reviewOnly ? "All caught up" : "No tasks yet") : "No matches"}
            message={
              props.reviewOnly
                ? "Submitted work will appear here, ready for a decision."
                : "Create a task with a clear goal and acceptance criteria."
            }
          />
        ) : (
          <div class="task-list" role="listbox" aria-label={props.reviewOnly ? "Needs review" : "Tasks"}>
            {filtered.map((task) => (
              <TaskRow
                key={String(task.id)}
                task={task}
                selected={store.selectedTask.value != null && idEq(store.selectedTask.value, task.id)}
                onSelect={() => store.selectTask(task.id)}
              />
            ))}
          </div>
        )}
        {snap?.truncated === true && (
          <div class="truncated-note">Showing the newest 1,000 tasks. Full history remains in the bus.</div>
        )}
      </div>
      <InspectorPane />
    </div>
  );
}

function TaskRow(props: { task: TaskRecord; selected: boolean; onSelect: () => void }) {
  const task = props.task;
  return (
    <button
      class={`task-row ${props.selected ? "selected" : ""}`}
      role="option"
      aria-selected={props.selected}
      onClick={props.onSelect}
    >
      <span class={`state-ico tone-${stateTone(task.state)}`}>
        <Icon name={stateIcon(task.state)} size={16} hidden />
      </span>
      <span style={{ minWidth: 0, flex: 1 }}>
        <span class="t-title">{task.title}</span>
        <span class="t-line">
          <span class="t-meta">{stateTitle(task.state)}</span>
          <span class="t-meta">#{String(task.id)}</span>
        </span>
        <span class="t-sub" style={{ display: "block" }}>{task.assignee ?? "Unassigned"}</span>
      </span>
    </button>
  );
}

function InspectorPane() {
  const detail = store.detail.value;
  const selected = store.selectedTask.value;
  if (store.detailLoading.value && (detail == null || selected == null || !idEq(detail.id, selected))) {
    return (
      <div class="inspector">
        <div class="empty-state">
          <span class="spinner" role="status" aria-label="Loading task" />
          <p>Loading task…</p>
        </div>
      </div>
    );
  }
  if (detail != null && selected != null && idEq(detail.id, selected)) {
    return <TaskInspector detail={detail} />;
  }
  if (store.detailError.value) {
    return (
      <div class="inspector">
        <EmptyState icon="exclamation-triangle" title="Couldn't load this task" message={store.detailError.value} />
      </div>
    );
  }
  return (
    <div class="inspector">
      <EmptyState
        icon="sidebar-right"
        title="The whole task, in one place"
        message="Select a task to see its brief, owner, submission, and review history."
      />
    </div>
  );
}

function TaskInspector(props: { detail: TaskDetail }) {
  const [decision, setDecision] = useState<TaskDecision | null>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  const task = props.detail;
  const canWrite = store.canWrite.value;
  const closeMenu = () => setMenuOpen(false);

  return (
    <div class="inspector">
      <div class="inspector-inner">
        <div>
          <div class="kicker">
            <span>TASK #{String(task.id)}</span>
            {task.priority !== "normal" && (
              <span style={{ marginLeft: "auto", textTransform: "capitalize", fontWeight: 400 }}>
                {task.priority} priority
              </span>
            )}
          </div>
          <h2 class="task-title">{task.title}</h2>
          <div class={`state-line tone-${stateTone(task.state)}`}>
            <Icon name={stateIcon(task.state)} size={17} hidden />
            <span>{stateTitle(task.state)}</span>
          </div>
          <p class="explain">{taskExplanation(task)}</p>
        </div>

        <div class="facts">
          <Fact label="Worker" value={task.assignee ?? "Not assigned"} />
          <Fact
            label="Reviewer"
            value={task.reviewer === "operator" ? "You (operator)" : task.reviewer ?? "Task creator / operator"}
          />
        </div>

        {task.state === "submitted" && (
          <div style={{ display: "flex", gap: 10 }}>
            <button class="btn primary" disabled={!canWrite} onClick={() => setDecision("accept")}>
              Accept Work
            </button>
            <button class="btn" disabled={!canWrite} onClick={() => setDecision("changes")}>
              Request Changes…
            </button>
          </div>
        )}

        <hr class="divider" />
        <ProseSection title="Task brief" text={task.brief || "No brief provided."} />
        <ProseSection
          title="Acceptance criteria"
          text={task.acceptance || "No acceptance criteria provided. Review the brief before accepting."}
        />

        {task.pathScopes.length > 0 && (
          <div>
            <h4 class="section-title">File scope</h4>
            <div class="checks">
              {task.pathScopes.map((scope) => (
                <span class="mono" style={{ fontSize: 13, userSelect: "text" }}>{scope}</span>
              ))}
            </div>
          </div>
        )}

        {task.result && (
          <>
            <hr class="divider" />
            <ProseSection title="Submitted work" text={task.result.summary} />
            {task.result.details !== "" && <ProseSection title="Details" text={task.result.details} />}
            {task.result.changedFiles.length > 0 && (
              <details class="details">
                <summary>Changed files ({task.result.changedFiles.length})</summary>
                <div class="details-body">
                  {task.result.changedFiles.map((file) => (
                    <span class="mono" style={{ fontSize: 12, userSelect: "text" }}>{file}</span>
                  ))}
                </div>
              </details>
            )}
            {task.result.validation.length > 0 && (
              <div>
                <h4 class="section-title">Worker-reported checks</h4>
                <div class="checks">
                  {task.result.validation.map((check, i) => (
                    <span class="check-row" key={i}>
                      <Icon
                        name={check.passed === true ? "checkmark-circle" : "exclamation-circle"}
                        size={15}
                        hidden
                        class={check.passed === true ? "tone-muted" : "tone-warn"}
                      />
                      <span class={check.passed === true ? "tone-muted" : ""}>{check.summary}</span>
                    </span>
                  ))}
                  <p class="fine">Reported by the worker, not independently verified by ACS.</p>
                </div>
              </div>
            )}
          </>
        )}

        {task.review && (
          <>
            <hr class="divider" />
            <ProseSection
              title={task.review.accepted ? `Accepted by ${task.review.reviewer}` : `Changes requested by ${task.review.reviewer}`}
              text={task.review.feedback || "No written feedback."}
            />
            <p class="fine">{timestamp(task.review.reviewedMs)}</p>
          </>
        )}

        {props.detail.notes.length > 0 && (
          <>
            <hr class="divider" />
            <div>
              <h4 class="section-title">Notes</h4>
              <div class="checks">
                {props.detail.notes.map((note) => (
                  <div class="note-block" key={String(note.id)}>
                    <div class="n-head">
                      {note.author} · {timestamp(note.tsMs)}
                    </div>
                    <div class="prose">{note.body}</div>
                  </div>
                ))}
              </div>
            </div>
          </>
        )}

        {props.detail.messages.length > 0 && (
          <>
            <hr class="divider" />
            <details class="details">
              <summary>Task messages ({props.detail.messages.length})</summary>
              <div class="details-body">
                {props.detail.messages.map((message) => (
                  <div key={String(message.seq)}>
                    <div class="n-head">
                      {message.sender} → {message.recipient ?? "Everyone"} · {timestamp(message.tsMs)}
                    </div>
                    <div style={{ fontWeight: 650, marginTop: 3 }}>{message.subject}</div>
                    <div class="prose" style={{ marginTop: 3 }}>{message.body}</div>
                  </div>
                ))}
              </div>
            </details>
          </>
        )}

        <hr class="divider" />
        <div class="inspector-footer">
          <span class="fine">Updated {timestamp(task.updatedMs)}</span>
          <span style={{ flex: 1 }} />
          {!isClosed(task.state) && (
            <div class="menu-wrap">
              <button class="btn" disabled={!canWrite} onClick={() => setMenuOpen((v) => !v)} aria-haspopup="menu" aria-expanded={menuOpen}>
                Task actions ▾
              </button>
              {menuOpen && (
                <div class="menu-list" role="menu">
                  {(task.state === "claimed" || task.state === "open") && (
                    <button role="menuitem" onClick={() => { closeMenu(); setDecision("requeue"); }}>
                      Return to Queue…
                    </button>
                  )}
                  <button role="menuitem" class="tone-bad" onClick={() => { closeMenu(); setDecision("cancel"); }}>
                    Cancel Task…
                  </button>
                </div>
              )}
            </div>
          )}
        </div>
      </div>
      {decision && <DecisionSheet task={task} decision={decision} onClose={() => setDecision(null)} />}
    </div>
  );
}

function Fact(props: { label: string; value: string }) {
  return (
    <div class="fact">
      <div class="f-label">{props.label}</div>
      <div class="f-value">{props.value}</div>
    </div>
  );
}

export function ProseSection(props: { title: string; text: string }) {
  return (
    <div>
      <h4 class="section-title">{props.title}</h4>
      <div class="prose">{props.text}</div>
    </div>
  );
}
