// Orchestration (AOS): goals from mission presets, editable mission and role
// prompts, and the crew file. Everything reads and writes through acs-desktop
// (orchestration, startGoal, saveMission, saveRole, setAgent).

import { useEffect, useMemo, useState } from "preact/hooks";
import { Icon } from "../icons";
import {
  expandMission,
  relativeTime,
  stateTitle,
  stateTone,
  type CrewMember,
  type MissionRecord,
  type RolePrompt,
} from "../model";
import { store } from "../store";

function useOrchestration() {
  useEffect(() => {
    if (store.orchestration.value == null) void store.loadOrchestration();
  }, [store.database.value]);
  return store.orchestration.value;
}

export function ModeHero(props: { kicker: string; title: string; lede: string; children?: preact.ComponentChildren }) {
  return (
    <header class="hero">
      <div class="hero-kicker">{props.kicker}</div>
      <div class="row-between" style={{ alignItems: "flex-end" }}>
        <div>
          <h1 class="hero-title">{props.title}</h1>
          <p class="hero-lede">{props.lede}</p>
        </div>
        <span style={{ flex: 1 }} />
        {props.children}
      </div>
    </header>
  );
}

// ------------------------------------------------------------------ goals

export function GoalsView() {
  const orch = useOrchestration();
  const [goal, setGoal] = useState("");
  const [mission, setMission] = useState("run");
  const [to, setTo] = useState("");
  const [showPlan, setShowPlan] = useState(false);
  const missions = orch?.missions ?? [];
  const picked = missions.find((m) => m.name === mission);
  const plan = picked && goal.trim() ? expandMission(picked, goal.trim()) : null;
  const agents = store.agents.value.filter((a) => !orch?.crew.some((m) => m.id === a.id && !m.enabled));
  useEffect(() => {
    if (to && orch?.crew.some((m) => m.id === to && !m.enabled)) setTo("");
  }, [orch, to]);
  const owner = to || orch?.goalOwner || null;

  const start = async () => {
    if (!picked || !goal.trim()) return;
    const ok = await store.mutate("startGoal", {
      mission: picked.name,
      goal: goal.trim(),
      to: to || null,
      project: store.project.value,
    });
    if (ok) {
      setGoal("");
      await store.loadOrchestration();
    }
  };

  return (
    <div class="view-scroll">
      <div class="view-pad orch">
        <ModeHero
          kicker="Orchestration / AOS"
          title="What should the crew do?"
          lede="Say the outcome in one line. Pick a preset and it becomes a full brief with acceptance criteria."
        />

        <section class="composer" aria-label="New goal">
          <textarea
            class="composer-input"
            value={goal}
            placeholder="e.g. the signup form accepts empty emails"
            onInput={(e) => setGoal((e.target as HTMLTextAreaElement).value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) void start();
            }}
            aria-label="Goal"
          />
          <div class="preset-chips" role="radiogroup" aria-label="Preset">
            {missions.map((m) => (
              <button
                key={m.name}
                role="radio"
                aria-checked={picked?.name === m.name}
                class={`chip ${picked?.name === m.name ? "on" : ""}`}
                onClick={() => setMission(m.name)}
                title={m.summary}
              >
                <span class="chip-name">{m.name}</span>
                {m.custom && <span class="chip-edit" aria-label="edited">*</span>}
              </button>
            ))}
          </div>
          {picked && <p class="composer-hint">{picked.summary}</p>}
          <div class="composer-foot">
            <label class="inline-select">
              <span>Hand to</span>
              <select value={to} onChange={(e) => setTo((e.target as HTMLSelectElement).value)} aria-label="Hand to">
                <option value="">{orch?.goalOwner ? `${orch.goalOwner} (crew lead)` : "First free agent"}</option>
                {agents.map((a) => (
                  <option key={a.id} value={a.id}>{a.id}</option>
                ))}
              </select>
            </label>
            <button class="link-btn" onClick={() => setShowPlan((v) => !v)} disabled={!plan} aria-expanded={showPlan}>
              {showPlan ? "Hide the brief" : "Preview the brief"}
            </button>
            <span style={{ flex: 1 }} />
            <span class="kbd-hint">Ctrl+Enter</span>
            <button class="btn primary big" disabled={!plan || !store.canWrite.value} onClick={() => void start()}>
              <Icon name="bolt" size={15} hidden /> Queue goal
            </button>
          </div>
          {plan && showPlan && (
            <div class="plan">
              <div class="plan-row"><span class="f-label">Title</span><div>{plan.title}</div></div>
              <div class="plan-row"><span class="f-label">Brief</span><pre>{plan.brief}</pre></div>
              {plan.acceptance && (
                <div class="plan-row"><span class="f-label">Done when</span><pre>{plan.acceptance}</pre></div>
              )}
            </div>
          )}
          <p class="fine">
            {owner ? `${owner} gets it as a task.` : "Any free agent can claim it."} Nothing starts an agent: if
            none is running, the goal waits in Tasks.
            {store.snapshot.value?.simulated ? " Sample workspace: agents are simulated." : ""}
          </p>
        </section>

        <section>
          <h2 class="sec-title">Recent goals <span class="g-count">{orch?.goals.length ?? 0}</span></h2>
          {orch == null ? (
            <p class="fine">{store.orchestrationError.value ?? "Loading…"}</p>
          ) : orch.goals.length === 0 ? (
            <p class="fine">No goals yet. Your first one shows up here and in Communication.</p>
          ) : (
            <ol class="goal-list">
              {orch.goals.map((g) => (
                <li key={String(g.id)}>
                  <button class="goal-row" onClick={() => store.openTask(g.id)}>
                    <span class="t-id">#{String(g.id)}</span>
                    <span class="goal-title">{g.title}</span>
                    <span class={`state-pill ${stateTone(g.state)}`}>{stateTitle(g.state)}</span>
                    <span class="goal-who">{g.assignee ?? "unclaimed"}</span>
                    <span class="m-time">{relativeTime(g.updatedMs)}</span>
                  </button>
                </li>
              ))}
            </ol>
          )}
        </section>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------- presets

type Kind = "mission" | "role";

export function PresetsView() {
  const orch = useOrchestration();
  const focus = store.presetFocus.value;
  const [kind, setKind] = useState<Kind>(focus?.kind ?? "mission");
  const [name, setName] = useState<string | null>(focus?.name ?? null);
  const [creating, setCreating] = useState(false);
  useEffect(() => {
    if (focus) {
      setKind(focus.kind);
      setName(focus.name);
      store.presetFocus.value = null;
    }
  }, [focus]);

  const items: (MissionRecord | RolePrompt)[] = kind === "mission" ? orch?.missions ?? [] : orch?.roles ?? [];
  const current = creating ? null : name === null ? items[0] ?? null : items.find((i) => i.name === name) ?? null;

  return (
    <div class="presets">
      <div class="presets-list">
        <div class="seg small" role="tablist" aria-label="Preset type">
          {(["mission", "role"] as Kind[]).map((k) => (
            <button
              key={k}
              role="tab"
              aria-selected={kind === k}
              class={kind === k ? "on" : ""}
              onClick={() => {
                setKind(k);
                setName(null);
                setCreating(false);
              }}
            >
              {k === "mission" ? "Goal presets" : "Agent roles"}
            </button>
          ))}
        </div>
        <p class="fine" style={{ margin: "10px 2px 12px" }}>
          {kind === "mission"
            ? "Templates a one-line goal expands into. {goal} is what you typed."
            : "What each agent is told at the start of every turn."}
        </p>
        <ul class="preset-list">
          {items.map((i) => (
            <li key={i.name}>
              <button
                class={`preset-item ${current?.name === i.name ? "on" : ""}`}
                onClick={() => {
                  setName(i.name);
                  setCreating(false);
                }}
              >
                <span class="p-title">{i.name}</span>
                <span class="p-tag">{i.custom ? "edited" : "built-in"}</span>
                {"summary" in i && <span class="p-sum">{i.summary}</span>}
              </button>
            </li>
          ))}
        </ul>
        {kind === "mission" && (
          <button class="btn" style={{ marginTop: 12 }} onClick={() => setCreating(true)} disabled={!store.canWrite.value}>
            <Icon name="plus" size={13} hidden /> New goal preset
          </button>
        )}
      </div>
      <div class="presets-editor">
        {orch == null ? (
          <p class="fine">{store.orchestrationError.value ?? "Loading…"}</p>
        ) : creating ? (
          <PresetEditor key="new" kind="mission" name="" text={NEW_MISSION} isNew onSaved={(n) => { setCreating(false); setName(n); }} />
        ) : current ? (
          <PresetEditor key={`${kind}:${current.name}`} kind={kind} name={current.name} text={current.text} onSaved={() => {}} />
        ) : (
          <p class="fine">Nothing here yet.</p>
        )}
      </div>
    </div>
  );
}

const NEW_MISSION = `# my-preset
One line that says what this preset is for.

## brief
The operator wants: {goal}

1. First step.
2. Second step.

## acceptance
- How you can tell it is done.
`;

function PresetEditor(props: { kind: Kind; name: string; text: string; isNew?: boolean; onSaved: (name: string) => void }) {
  const [text, setText] = useState(props.text);
  const [name, setName] = useState(props.name);
  const dirty = text !== props.text || name !== props.name;
  const derived = useMemo(() => {
    if (!props.isNew) return name;
    const m = /^#\s*([A-Za-z0-9_-]+)/m.exec(text);
    return name || (m?.[1] ? m[1].toLowerCase() : "");
  }, [text, name, props.isNew]);

  const save = async () => {
    const ok = await store.mutate(props.kind === "mission" ? "saveMission" : "saveRole", { name: derived, text });
    if (ok) {
      await store.loadOrchestration();
      props.onSaved(derived);
    }
  };

  return (
    <div class="editor">
      <div class="row-between">
        <div>
          <div class="hero-kicker">{props.kind === "mission" ? "Goal preset" : "Agent role"}</div>
          {props.isNew ? (
            <input
              class="editor-name"
              value={derived}
              onInput={(e) => setName((e.target as HTMLInputElement).value)}
              aria-label="Preset name"
              placeholder="name"
            />
          ) : (
            <h2 class="editor-title">{props.name}</h2>
          )}
        </div>
        <span style={{ flex: 1 }} />
        {dirty && !props.isNew && (
          <button class="btn" onClick={() => { setText(props.text); setName(props.name); }}>
            Discard
          </button>
        )}
        <button class="btn primary" disabled={!dirty || !derived || !store.canWrite.value} onClick={() => void save()}>
          Save
        </button>
      </div>
      <textarea
        class="editor-text"
        value={text}
        spellcheck={false}
        onInput={(e) => setText((e.target as HTMLTextAreaElement).value)}
        aria-label={`${props.kind} prompt`}
      />
      <p class="fine">
        Saved as a plain file in the workspace’s aos folder; the terminal tools read the same file.
        {props.kind === "role" ? " Agents pick up edits on their next turn." : ""}
      </p>
    </div>
  );
}

// ------------------------------------------------------------------- crew

export function CrewPanel() {
  const orch = useOrchestration();
  if (orch == null) return null;
  if (orch.crewError) {
    return <p class="form-error">crew.json does not load: {orch.crewError}</p>;
  }
  if (!orch.configured) {
    return (
      <div class="crew-empty">
        <Icon name="people" size={20} hidden />
        <div>
          <strong>No crew file yet.</strong>{" "}
          {orch.simulated
            ? "The sample workspace uses simulated agents, so there is nothing to configure here."
            : "Set up a local crew below to choose which CLIs work as builder and reviewer."}
        </div>
      </div>
    );
  }
  return (
    <section class="crew">
      <h2 class="sec-title">Crew <span class="g-count">{orch.crew.length}</span></h2>
      <div class="crew-grid">
        {orch.crew.map((m) => (
          <CrewCard key={m.id} member={m} lead={orch.goalOwner === m.id} />
        ))}
      </div>
    </section>
  );
}

function CrewCard(props: { member: CrewMember; lead: boolean }) {
  const m = props.member;
  const [editing, setEditing] = useState(false);
  const [desc, setDesc] = useState(m.description);
  const canWrite = store.canWrite.value;
  const roleFile = m.instructions?.match(/^roles\/([a-zA-Z0-9_-]+)\.md$/)?.[1];
  const editableRole = roleFile && store.orchestration.value?.roles.some((role) => role.name === roleFile);
  const save = async () => {
    if (await store.mutate("setAgent", { id: m.id, description: desc })) {
      setEditing(false);
      await store.loadOrchestration();
    }
  };
  return (
    <div class={`crew-card ${m.enabled ? "" : "off"}`}>
      <div class="cc-head">
        <span class="a-badge" aria-hidden="true">{(m.cli || m.id).charAt(0).toUpperCase()}</span>
        <div style={{ minWidth: 0, flex: 1 }}>
          <div class="a-id">{m.id}{props.lead && <span class="lead-tag">gets goals</span>}</div>
          <div class="a-meta">{m.role} · {m.cli || "no CLI"}{m.running ? " · running" : ""}</div>
        </div>
        <label class="switch" title={m.enabled ? "In the crew" : "Left out"}>
          <input
            type="checkbox"
            checked={m.enabled}
            disabled={!canWrite}
            onChange={async (e) => {
              await store.mutate("setAgent", { id: m.id, enabled: (e.target as HTMLInputElement).checked });
              await store.loadOrchestration();
            }}
            aria-label={`${m.id} in the crew`}
          />
          <span class="track" aria-hidden="true" />
        </label>
      </div>
      {editing ? (
        <div class="cc-edit">
          <input value={desc} onInput={(e) => setDesc((e.target as HTMLInputElement).value)} aria-label="Description" />
          <button class="btn" onClick={() => setEditing(false)}>Cancel</button>
          <button class="btn primary" onClick={() => void save()} disabled={!canWrite}>Save</button>
        </div>
      ) : (
        <p class="cc-desc">{m.description || "No description."}</p>
      )}
      <div class="cc-actions">
        <button class="link-btn" onClick={() => setEditing(true)} disabled={!canWrite}>
          <Icon name="pencil" size={12} hidden /> Describe
        </button>
        <button
          class="link-btn"
          disabled={!editableRole}
          title={editableRole ? undefined : `Edit the configured instructions file directly: ${m.instructions ?? "none configured"}`}
          onClick={() => {
            if (!roleFile || !editableRole) return;
            store.presetFocus.value = { kind: "role", name: roleFile };
            store.go("presets");
          }}
        >
          <Icon name="sliders" size={12} hidden /> Edit role prompt
        </button>
      </div>
    </div>
  );
}
