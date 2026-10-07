// WorkspaceView equivalent: sidebar + banners + destination pane + statusbar.
// Keyboard accelerators mirror the macOS menu commands (Ctrl+N, Ctrl+O, Ctrl+R).

import { useEffect, useRef, useState } from "preact/hooks";
import { Icon, Mark } from "./icons";
import { MODES, store, type DestinationInfo } from "./store";
import { Welcome } from "./views/Welcome";
import { TaskBrowser } from "./views/Tasks";
import { AgentsView } from "./views/Agents";
import { MessagesView } from "./views/Messages";
import { NewTaskSheet } from "./views/Sheets";
import { GoalsView, PresetsView } from "./views/Orchestration";
import { taskGroup } from "./model";

export function App() {
  const snap = store.snapshot.value;
  const dest = store.destination.value;

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey)) return;
      const key = e.key.toLowerCase();
      if (document.querySelector("dialog[open]")) {
        if (["n", "o", "1", "2", "r"].includes(key)) e.preventDefault();
        return;
      }
      if (key === "n" && store.canWrite.value) {
        e.preventDefault();
        store.showNewTask.value = true;
      } else if (key === "o") {
        e.preventDefault();
        void store.chooseProject();
      } else if (key === "1" || key === "2") {
        e.preventDefault();
        store.switchMode(key === "1" ? "communication" : "orchestration");
      } else if (key === "r") {
        e.preventDefault();
        void store.refresh();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div class="app-outer">
      {store.previewBuild.value && (
        <div class="preview-banner" role="status">
          Preview build — not connected to a real ACS workspace
        </div>
      )}
      <div class="app-shell">
      <Sidebar />
      <div class={`main mode-${store.mode.value}`}>
        <ModeBar />
        <div class="main-body">
          {snap?.simulated === true && (
            <Banner tone="accent" icon="sparkles" text="Sample workspace · simulated agents, no model calls or charges" />
          )}
          {store.error.value && (
            <Banner tone="err" icon="exclamation-triangle" text={store.error.value} onDismiss={() => (store.error.value = null)} />
          )}
          {!store.error.value && store.notice.value && (
            <Banner tone="ok" icon="checkmark-circle" text={store.notice.value} onDismiss={() => (store.notice.value = null)} />
          )}
          {snap == null ? (
            <Welcome />
          ) : (
            <>
              {snap.canOperate === false && (
                <Banner tone="warn" icon="lock" text="Read-only connection. The operator identity is unavailable for this bus." />
              )}
              {dest === "tasks" || dest === "reviews" ? (
                <TaskBrowser reviewOnly={dest === "reviews"} />
              ) : dest === "agents" ? (
                <AgentsView />
              ) : dest === "goals" ? (
                <GoalsView />
              ) : dest === "presets" ? (
                <PresetsView />
              ) : (
                <>
                  <Pulse />
                  <MessagesView />
                </>
              )}
            </>
          )}
        </div>
        <div class="statusbar">
          {(store.busy.value || store.refreshing.value) && <span class="spinner" role="status" aria-label="Working" />}
          <span>{store.busy.value ? "Working…" : "Your tasks and history stay on your PC."}</span>
          <span class="right">
            {store.lastRefresh.value &&
              `Updated ${store.lastRefresh.value.toLocaleTimeString(undefined, {
                hour: "numeric",
                minute: "2-digit",
                second: "2-digit",
              })}`}
          </span>
        </div>
      </div>
      {store.showNewTask.value && <NewTaskSheet />}
      </div>
    </div>
  );
}

function Sidebar() {
  const [menuOpen, setMenuOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);
  const snap = store.snapshot.value;

  useEffect(() => {
    if (!menuOpen) return;
    const onDown = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) setMenuOpen(false);
    };
    window.addEventListener("mousedown", onDown);
    return () => window.removeEventListener("mousedown", onDown);
  }, [menuOpen]);

  const mode = MODES.find((m) => m.id === store.mode.value) ?? MODES[0]!;
  const destButton = (d: DestinationInfo) => (
    <button
      key={d.id}
      class={`nav-item ${store.destination.value === d.id ? "active" : ""}`}
      onClick={() => store.go(d.id)}
      aria-current={store.destination.value === d.id ? "page" : undefined}
    >
      <Icon name={d.icon} size={17} hidden />
      <span>{d.label}</span>
      {d.id === "reviews" && store.reviewCount.value > 0 && (
        <span class="count">{store.reviewCount.value}</span>
      )}
    </button>
  );

  return (
    <aside class="sidebar">
      <div class="sidebar-brand">
        <Mark size={22} />
        <div>
          <div class="brand-title">ACS</div>
          <div class="brand-sub">Agent workspace</div>
        </div>
      </div>
      <nav aria-label={mode.label}>
        <div class="nav-caption">{mode.blurb}</div>
        {mode.destinations.map(destButton)}
      </nav>
      <div class="sidebar-footer">
        {snap && store.destination.value !== "agents" && (
          <button class="agent-settings" onClick={() => store.go("agents")}>
            <Icon name="sliders" size={16} hidden /> Configure agents
          </button>
        )}
        <div class="ws-name">
          <Icon name={snap?.simulated === true ? "sparkles" : "folder"} size={15} hidden />
          <span>{store.name.value}</span>
        </div>
        <div class="ws-switch" ref={menuRef}>
          <button
            class="ws-menu-btn"
            onClick={() => setMenuOpen((v) => !v)}
            disabled={store.busy.value}
            aria-haspopup="menu"
            aria-expanded={menuOpen}
          >
            Switch workspace ▾
          </button>
          {menuOpen && (
            <div class="ws-menu" role="menu">
              <button role="menuitem" onClick={() => { setMenuOpen(false); void store.chooseProject(); }}>
                Open Project…
              </button>
              <button role="menuitem" onClick={() => { setMenuOpen(false); void store.chooseDatabase(); }}>
                Connect Existing Bus…
              </button>
              <button role="menuitem" onClick={() => { setMenuOpen(false); void store.openSample(); }}>
                Try Sample Workspace
              </button>
              <hr />
              <button
                role="menuitem"
                disabled={store.database.value == null}
                onClick={() => { setMenuOpen(false); void store.revealDatabase(); }}
              >
                Show Database in Explorer
              </button>
            </div>
          )}
        </div>
        <div class="local-note">
          <Icon name="drive" size={13} hidden />
          <span>Local on this PC</span>
        </div>
      </div>
    </aside>
  );
}

export function Banner(props: {
  tone: "accent" | "err" | "ok" | "warn";
  icon: string;
  text: string;
  onDismiss?: () => void;
}) {
  return (
    <div class={`banner ${props.tone}`} role={props.tone === "err" ? "alert" : "status"}>
      <Icon name={props.icon} size={16} hidden />
      <span class="text">{props.text}</span>
      {props.onDismiss && (
        <button class="dismiss" onClick={props.onDismiss} aria-label="Dismiss message">
          <Icon name="xmark" size={13} hidden />
        </button>
      )}
    </div>
  );
}

export function EmptyState(props: { icon: string; title: string; message: string }) {
  return (
    <div class="empty-state">
      <Icon name={props.icon} size={38} hidden />
      <h3>{props.title}</h3>
      <p>{props.message}</p>
    </div>
  );
}

/// The Chat/Cowork-style switch: two modes over the same workspace.
function ModeBar() {
  const current = store.mode.value;
  return (
    <div class="modebar">
      <div class="mode-switch" role="group" aria-label="Mode">
        {MODES.map((m, i) => (
          <button
            key={m.id}
            aria-pressed={current === m.id}
            class={`mode-btn ${m.id} ${current === m.id ? "on" : ""}`}
            onClick={() => store.switchMode(m.id)}
            title={`${m.blurb} (Ctrl+${i + 1})`}
          >
            <span class="mode-code">{m.code}</span>
            <span class="mode-label">{m.label}</span>
          </button>
        ))}
      </div>
      <span class="modebar-ws">{store.name.value}</span>
    </div>
  );
}

/// Communication dashboard header: what needs you and what is moving.
function Pulse() {
  const tasks = store.tasks.value;
  const count = (g: string) => tasks.filter((t) => taskGroup(t.state) === g).length;
  const agents = store.agents.value;
  const live = agents.filter((a) => a.running && !a.paused).length;
  const tiles: { label: string; value: string; hint: string; tone: string; go?: () => void }[] = [
    { label: "Needs you", value: String(count("review")), hint: "submitted, waiting for review", tone: count("review") ? "hot" : "", go: () => store.go("reviews") },
    { label: "Stuck", value: String(count("stuck")), hint: "blocked or failed", tone: count("stuck") ? "bad" : "", go: () => store.go("tasks") },
    { label: "Moving", value: String(count("active")), hint: "claimed by an agent", tone: "", go: () => store.go("tasks") },
    { label: "Queued", value: String(count("queued")), hint: "waiting for a free agent", tone: "", go: () => store.go("tasks") },
    { label: "Agents", value: `${live}/${agents.length}`, hint: "running now", tone: "", go: () => store.go("agents") },
  ];
  return (
    <div class="pulse" role="list" aria-label="Workspace at a glance">
      {tiles.map((t) => (
        <button key={t.label} role="listitem" class={`pulse-tile ${t.tone}`} onClick={t.go}>
          <span class="pt-label">{t.label}</span>
          <span class="pt-value">{t.value}</span>
          <span class="pt-hint">{t.hint}</span>
        </button>
      ))}
    </div>
  );
}
