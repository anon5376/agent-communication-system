// AgentsView — crew list, start/stop/pause controls, CLI detection and the
// local-crew setup confirmation, matching AgentsView.swift's gating and copy.

import { useState } from "preact/hooks";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { Icon } from "../icons";
import { relativeTime, type AgentRecord } from "../model";
import { store } from "../store";
import { CrewPanel } from "./Orchestration";

export function AgentsView() {
  const [starting, setStarting] = useState<AgentRecord | null>(null);
  const [stopping, setStopping] = useState<AgentRecord | null>(null);
  const [confirmSetup, setConfirmSetup] = useState(false);
  const agents = store.agents.value;
  const providers = store.providers.value;
  const simulated = store.snapshot.value?.simulated === true;
  const canWrite = store.canWrite.value;

  return (
    <div class="view-scroll">
      <div class="view-pad">
        <div class="row-between">
          <div>
            <div class="hero-kicker">Orchestration / AOS</div>
            <h1 class="view-title">Your agents</h1>
            <p class="view-sub">Who is in the crew, what each one is told, and whether it is running.</p>
          </div>
          <span style={{ flex: 1 }} />
          <button class="btn" onClick={() => void store.detect()} disabled={store.busy.value}>
            Find Installed CLIs
          </button>
        </div>

        <CrewPanel />

        <h2 class="sec-title" style={{ marginTop: 26 }}>On the bus</h2>
        {agents.length === 0 ? (
          <div style={{ padding: "20px 0", display: "flex", flexDirection: "column", gap: 14 }}>
            <h2 style={{ fontSize: 21, fontWeight: 600, margin: 0 }}>Start with the tools you already use.</h2>
            <p class="view-sub" style={{ maxWidth: 640 }}>
              ACS can detect supported coding CLIs on this PC and create a local crew configuration.
              Nothing starts automatically. Sign in to each provider separately using its own CLI.
            </p>
            <div>
              <button
                class="btn primary"
                disabled={!canWrite || simulated}
                onClick={() => setConfirmSetup(true)}
              >
                Set Up Local Crew…
              </button>
            </div>
          </div>
        ) : (
          <div>
            {agents.map((agent) => (
              <div class="agent-row" key={agent.id}>
                <span class="a-badge" aria-hidden="true">
                  {agent.harness.charAt(0).toUpperCase()}
                </span>
                <div style={{ minWidth: 0 }}>
                  <div class="a-id">{agent.id}</div>
                  <div class="a-meta">
                    {agent.role} · {agent.harness} · {agent.model}
                  </div>
                </div>
                <div class={`a-state ${agent.paused ? "paused" : agent.running ? "running" : "stopped"}`}>
                  <span class="a-dot" aria-hidden="true" />
                  <span>
                    <span class="a-word">{agent.paused ? "Paused" : agent.running ? "Running" : "Not running"}</span>
                    <span class="a-seen">
                      {agent.lastSeenMs != null ? `Seen ${relativeTime(agent.lastSeenMs)}` : "Not seen on the bus yet"}
                    </span>
                  </span>
                </div>
                <div class="a-actions">
                  {agent.running ? (
                    <>
                      <button
                        class="btn"
                        disabled={!canWrite}
                        onClick={() => void store.mutate(agent.paused ? "resume" : "pause", { id: agent.id })}
                      >
                        {agent.paused ? "Resume" : "Pause"}
                      </button>
                      <button class="btn danger" disabled={!canWrite} onClick={() => setStopping(agent)}>
                        Stop…
                      </button>
                    </>
                  ) : (
                    <span class="a-start">
                      <button class="btn" disabled={!canWrite || simulated} onClick={() => setStarting(agent)}>
                        Start…
                      </button>
                      {simulated && <span class="fine">Sample agents can’t start</span>}
                    </span>
                  )}
                </div>
              </div>
            ))}
            <p class="fine" style={{ marginTop: 16 }}>
              “Running” means ACS is managing that agent’s process. It doesn’t prove the provider is
              signed in or making progress. “Seen” is the agent’s last activity on the bus, which can
              also come from a CLI started elsewhere.
            </p>
          </div>
        )}

        {providers.length > 0 && (
          <>
            <hr class="divider" style={{ margin: "24px 0" }} />
            <h2 class="view-title" style={{ fontSize: 21 }}>CLI detection</h2>
            <p class="view-sub">Installed is not authenticated. These checks do not make model calls.</p>
            <div style={{ marginTop: 12 }}>
              {providers.map((provider) => (
                <div class="provider-row" key={provider.id}>
                  <Icon name={provider.path == null ? "minus-circle" : "checkmark-circle"} size={18} hidden />
                  <div style={{ flex: 1, minWidth: 0 }}>
                    <div class="p-head">
                      <span class="p-name">{provider.name}</span>
                      <span class="p-state">
                        {provider.path == null ? "Not installed" : "Installed · authentication unverified"}
                      </span>
                    </div>
                    {provider.path != null && <div class="p-path mono">{provider.path}</div>}
                    {provider.signIn !== "" && <div class="p-signin">{provider.signIn}</div>}
                    {provider.requiresApproval && (
                      <div class="p-approval">
                        This integration requires explicit approval in crew configuration before starting.
                      </div>
                    )}
                  </div>
                </div>
              ))}
            </div>
          </>
        )}

        {agents.length > 0 && !simulated && (
          <div style={{ marginTop: 24 }}>
            <button class="btn" disabled={!canWrite} onClick={() => setConfirmSetup(true)}>
              Set Up Missing Crew Configuration…
            </button>
          </div>
        )}
      </div>

      {starting && <StartAgentSheet agent={starting} onClose={() => setStarting(null)} />}

      {confirmSetup && (
        <AlertSheet
          title="Set up a local crew?"
          message="ACS will detect installed CLIs and create missing crew files and agent identities in this workspace. Existing custom configuration is preserved. No providers will run."
          confirmLabel="Set Up"
          onCancel={() => setConfirmSetup(false)}
          onConfirm={() => {
            setConfirmSetup(false);
            void (async () => {
              await store.mutate("setup");
              await store.detect();
            })();
          }}
        />
      )}

      {stopping && (
        <AlertSheet
          title="Stop this agent?"
          message="This stops its managed process. Task history remains; review any claimed work before returning it to the queue."
          confirmLabel="Stop Agent"
          cancelLabel="Keep Running"
          danger
          onCancel={() => setStopping(null)}
          onConfirm={() => {
            const id = stopping.id;
            setStopping(null);
            void store.mutate("stop", { ids: [id] });
          }}
        />
      )}
    </div>
  );
}

function StartAgentSheet(props: { agent: AgentRecord; onClose: () => void }) {
  const [folder, setFolder] = useState<string | null>(store.project.value);
  const [approved, setApproved] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const busy = store.busy.value;

  const chooseFolder = async () => {
    const picked = await openDialog({ title: "Choose Project", directory: true, multiple: false });
    if (typeof picked === "string" && picked) {
      setFolder(picked);
      setApproved(false);
    }
  };

  const start = async () => {
    if (!folder) return;
    const ok = await store.mutate("start", {
      ids: [props.agent.id],
      workdir: folder,
      confirmed: approved,
    });
    if (ok) props.onClose();
    else setFailure(store.error.value ?? "The agent could not be started.");
  };

  return (
    <SheetShell width={580} onClose={props.onClose}>
      <h2>Start {props.agent.id}</h2>
      <p style={{ fontWeight: 650, margin: 0 }}>This starts a real coding agent.</p>
      <p class="sheet-sub">
        It may send project content to its provider, modify files, run commands, and incur charges
        under your provider account. ACS coordination controls are not a sandbox or a guaranteed
        dollar cap.
      </p>
      <div class="field">
        <span class="f-label">Project folder</span>
        <div style={{ display: "flex", gap: 12, alignItems: "center" }}>
          <span class="mono" style={{ flex: 1, fontSize: 13, userSelect: "text", overflowWrap: "anywhere" }}>
            {folder ?? "Choose a project folder"}
          </span>
          <button class="btn" onClick={() => void chooseFolder()} disabled={busy}>
            Choose…
          </button>
        </div>
      </div>
      <label class="trust-row">
        <input type="checkbox" checked={approved} onChange={(e) => setApproved((e.target as HTMLInputElement).checked)} />
        <span>I trust this project and approve this agent running in it.</span>
      </label>
      <p class="inline-note">The agent keeps running if you close ACS. Use Stop in Agents to stop its supervisor.</p>
      {failure && <p class="form-error">{failure}</p>}
      <div class="actions">
        <span class="spacer" />
        <button class="btn" onClick={props.onClose} disabled={busy}>
          Cancel
        </button>
        <button class="btn primary" onClick={() => void start()} disabled={!approved || folder == null || !store.canWrite.value}>
          Start Agent
        </button>
      </div>
    </SheetShell>
  );
}

function AlertSheet(props: {
  title: string;
  message: string;
  confirmLabel: string;
  cancelLabel?: string;
  danger?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return (
    <SheetShell width={430} onClose={props.onCancel}>
      <h2>{props.title}</h2>
      <p class="sheet-sub">{props.message}</p>
      <div class="actions">
        <span class="spacer" />
        <button class="btn" onClick={props.onCancel}>
          {props.cancelLabel ?? "Cancel"}
        </button>
        <button class={`btn primary ${props.danger ? "danger" : ""}`} onClick={props.onConfirm} autofocus>
          {props.confirmLabel}
        </button>
      </div>
    </SheetShell>
  );
}

// Shared modal shell (same behavior as Sheets.tsx's Sheet, kept local so this
// file stays self-contained like the Swift views).
import { useEffect } from "preact/hooks";

function SheetShell(props: { width: number; onClose: () => void; children: preact.ComponentChildren }) {
  const busy = store.busy.value;
  useEffect(() => {
    if (busy) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") props.onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [busy, props.onClose]);
  return (
    <div
      class="sheet-scrim"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget && !busy) props.onClose();
      }}
    >
      <div class="sheet" role="dialog" aria-modal="true" style={{ width: props.width }}>
        {props.children}
      </div>
    </div>
  );
}
