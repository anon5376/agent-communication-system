// WorkspaceStore — the app's state owner, mirroring WorkspaceStore.swift.
// Same lifecycle: connect -> snapshot -> refresh loop; every mutation is
// followed by an authoritative snapshot refresh rather than local guessing.

import { computed, signal } from "@preact/signals";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  acsRequest,
  acsBuildFlavor,
  sampleDbPath,
  workspaceDbPath,
  revealDatabase,
  type AcsError,
} from "./client";
import {
  parseOrchestration,
  parseProviders,
  parseSnapshot,
  parseTaskDetail,
  type Acknowledgement,
  type AgentRecord,
  type Id,
  type Orchestration,
  type ProviderRecord,
  type Snapshot,
  type TaskDetail,
  type TaskRecord,
} from "./model";

export type Mode = "communication" | "orchestration";
export type Destination = "messages" | "tasks" | "reviews" | "goals" | "presets" | "agents";

export interface DestinationInfo {
  id: Destination;
  label: string;
  icon: string;
}

/// Communication (ACS) is watching and answering the work; Orchestration
/// (AOS) is deciding what the crew does and how it is set up.
export const MODES: { id: Mode; label: string; code: string; blurb: string; destinations: DestinationInfo[] }[] = [
  {
    id: "communication",
    label: "Communication",
    code: "ACS",
    blurb: "Watch messages, tasks and reviews",
    destinations: [
      { id: "messages", label: "Live feed", icon: "chat" },
      { id: "tasks", label: "Tasks", icon: "checklist" },
      { id: "reviews", label: "Needs review", icon: "tray" },
    ],
  },
  {
    id: "orchestration",
    label: "Orchestration",
    code: "AOS",
    blurb: "Set goals, prompts and agents",
    destinations: [
      { id: "goals", label: "Goals", icon: "target" },
      { id: "presets", label: "Prompt presets", icon: "sliders" },
      { id: "agents", label: "Agents", icon: "people" },
    ],
  },
];

export const DESTINATIONS: DestinationInfo[] = MODES.flatMap((m) => m.destinations);

export function modeOf(dest: Destination): Mode {
  return MODES.find((m) => m.destinations.some((d) => d.id === dest))?.id ?? "communication";
}

const MODE_KEY = "acs.mode";

const DB_KEY = "acs.database";
const PROJECT_KEY = "acs.project";

function errorText(raw: unknown): string {
  if (raw instanceof Error) return raw.message;
  if (typeof raw === "string") return raw;
  return "Something went wrong talking to the ACS helper.";
}

export class WorkspaceStore {
  snapshot = signal<Snapshot | null>(null);
  detail = signal<TaskDetail | null>(null);
  providers = signal<ProviderRecord[]>([]);
  destination = signal<Destination>(
    localStorage.getItem(MODE_KEY) === "orchestration" ? "goals" : "messages",
  );
  orchestration = signal<Orchestration | null>(null);
  orchestrationError = signal<string | null>(null);
  /// Preset the Presets screen should open on (set from elsewhere, e.g. Agents).
  presetFocus = signal<{ kind: "mission" | "role"; name: string } | null>(null);
  selectedTask = signal<Id | null>(null);
  database = signal<string | null>(null);
  project = signal<string | null>(null);
  error = signal<string | null>(null);
  notice = signal<string | null>(null);
  busy = signal(false);
  refreshing = signal(false);
  detailLoading = signal(false);
  detailError = signal<string | null>(null);
  showNewTask = signal(false);
  lastRefresh = signal<Date | null>(null);
  /// True when the bundled helper is the preview double, not the real bridge.
  previewBuild = signal(false);

  private generation = 0;
  private restored = false;

  mode = computed<Mode>(() => modeOf(this.destination.value));

  canWrite = computed(() => this.snapshot.value?.canOperate === true && !this.busy.value);
  name = computed(() => {
    const snap = this.snapshot.value;
    if (snap?.simulated) return "Sample workspace";
    const proj = this.project.value;
    if (proj) return baseName(proj);
    const db = this.database.value;
    if (db) return baseName(parentDir(db)) || "Your workspace";
    return "Your workspace";
  });
  agents = computed<AgentRecord[]>(
    () => this.snapshot.value?.agents.filter((a) => a.id !== "operator") ?? [],
  );
  tasks = computed<TaskRecord[]>(() => this.snapshot.value?.tasks ?? []);
  reviewCount = computed(
    () => this.tasks.value.filter((t) => t.state === "submitted").length,
  );

  /** Restore the last workspace (same persistence keys as the macOS app). */
  async restore(): Promise<void> {
    if (this.restored) return;
    this.restored = true;
    try {
      this.previewBuild.value = (await acsBuildFlavor()) === "fake";
    } catch {
      this.previewBuild.value = false;
    }
    const dbPath = localStorage.getItem(DB_KEY);
    if (!dbPath) return;
    const folder = localStorage.getItem(PROJECT_KEY);
    await this.connect(dbPath, folder || null);
  }

  async chooseProject(): Promise<void> {
    if (this.busy.value) return;
    const folder = await openDialog({
      title: "Choose a project for ACS",
      directory: true,
      multiple: false,
    });
    if (typeof folder !== "string" || !folder) return;
    const db = await workspaceDbPath(folder);
    const initialize = true; // desktop init creates new buses without rotating existing operator tokens
    await this.connect(db, folder, { initialize });
  }

  async chooseDatabase(): Promise<void> {
    if (this.busy.value) return;
    const picked = await openDialog({
      title: "Connect an existing ACS bus",
      directory: false,
      multiple: false,
      filters: [{ name: "ACS bus", extensions: ["db"] }],
    });
    if (typeof picked !== "string" || !picked) return;
    await this.connect(picked, null);
  }

  async openSample(): Promise<void> {
    if (this.busy.value) return;
    const db = await sampleDbPath();
    await this.connect(db, null, { demo: true });
  }

  private async connect(
    db: string,
    folder: string | null,
    opts: { initialize?: boolean; demo?: boolean } = {},
  ): Promise<void> {
    if (this.busy.value) return;
    this.busy.value = true;
    this.error.value = null;
    try {
      if (opts.initialize || opts.demo) {
        // Desktop init creates a new bus or connects to an existing one without changing its operator token.
        await acsRequest<Acknowledgement>(db, opts.demo ? "demo" : "init");
      }
      const raw = await acsRequest<unknown>(db, "snapshot");
      const state = parseSnapshot(raw);
      this.generation += 1;
      this.database.value = db;
      this.project.value = folder;
      this.snapshot.value = state;
      this.providers.value = [];
      this.detail.value = null;
      this.detailError.value = null;
      this.detailLoading.value = false;
      this.selectedTask.value = null;
      this.notice.value = null;
      this.orchestration.value = null;
      this.destination.value = this.mode.value === "orchestration" ? "goals" : "messages";
      this.lastRefresh.value = new Date();
      localStorage.setItem(DB_KEY, db);
      if (folder) localStorage.setItem(PROJECT_KEY, folder);
      else localStorage.removeItem(PROJECT_KEY);
    } catch (raw) {
      this.error.value = errorText(raw);
    } finally {
      this.busy.value = false;
    }
  }

  async refresh(): Promise<void> {
    const db = this.database.value;
    if (!db || this.refreshing.value || this.busy.value) return;
    const gen = this.generation;
    this.refreshing.value = true;
    try {
      const raw = await acsRequest<unknown>(db, "snapshot");
      const state = parseSnapshot(raw);
      if (gen !== this.generation) return;
      this.snapshot.value = state;
      this.lastRefresh.value = new Date();
      this.error.value = null;
      await this.loadDetail();
      if (this.mode.value === "orchestration") await this.loadOrchestration();
    } catch (raw) {
      if (gen === this.generation) this.error.value = errorText(raw);
    } finally {
      this.refreshing.value = false;
    }
  }

  async loadDetail(): Promise<void> {
    const id = this.selectedTask.value;
    const db = this.database.value;
    if (id == null || !db) {
      this.detail.value = null;
      return;
    }
    const gen = this.generation;
    this.detailLoading.value = true;
    this.detailError.value = null;
    try {
      const raw = await acsRequest<unknown>(db, "task", { id });
      const next = parseTaskDetail(raw);
      if (gen !== this.generation || !sameId(this.selectedTask.value, id)) return;
      this.detail.value = next;
    } catch (raw) {
      if (gen !== this.generation || !sameId(this.selectedTask.value, id)) return;
      this.detail.value = null;
      this.detailError.value = errorText(raw);
    } finally {
      if (gen === this.generation && sameId(this.selectedTask.value, id)) {
        this.detailLoading.value = false;
      }
    }
  }

  /** Run a mutating action, then reload authoritative state. Returns success. */
  async mutate(action: string, payload: Record<string, unknown> = {}): Promise<boolean> {
    const db = this.database.value;
    if (!db || !this.canWrite.value) return false;
    this.busy.value = true;
    this.error.value = null;
    this.notice.value = null;
    try {
      const reply = await acsRequest<Acknowledgement>(db, action, payload);
      this.notice.value = reply.message;
      this.busy.value = false;
      await this.refresh();
      return true;
    } catch (raw) {
      this.busy.value = false;
      this.error.value = errorText(raw as AcsError);
      // A multi-agent action can partially succeed; always reload
      // authoritative state before showing the error.
      const actionError = this.error.value;
      await this.refresh();
      this.error.value = actionError;
      return false;
    }
  }

  async loadOrchestration(): Promise<void> {
    const db = this.database.value;
    if (!db) return;
    const gen = this.generation;
    try {
      const raw = await acsRequest<unknown>(db, "orchestration");
      if (gen !== this.generation) return;
      this.orchestration.value = parseOrchestration(raw);
      this.orchestrationError.value = null;
    } catch (raw) {
      if (gen === this.generation) this.orchestrationError.value = errorText(raw);
    }
  }

  go(dest: Destination): void {
    this.destination.value = dest;
    localStorage.setItem(MODE_KEY, modeOf(dest));
    if (modeOf(dest) === "orchestration") void this.loadOrchestration();
  }

  switchMode(mode: Mode): void {
    if (this.mode.value === mode) return;
    this.go(mode === "orchestration" ? "goals" : "messages");
  }

  /** Open a task in Communication (e.g. from a goal). */
  openTask(id: Id): void {
    this.go("tasks");
    this.selectTask(id);
  }

  async detect(): Promise<void> {
    const db = this.database.value;
    if (!db || this.busy.value) return;
    const gen = this.generation;
    this.busy.value = true;
    try {
      const raw = await acsRequest<unknown>(db, "detect");
      if (gen === this.generation) this.providers.value = parseProviders(raw);
    } catch (raw) {
      this.error.value = errorText(raw);
    } finally {
      this.busy.value = false;
    }
  }

  async revealDatabase(): Promise<void> {
    const db = this.database.value;
    if (!db) return;
    try {
      await revealDatabase(db);
    } catch {
      // Revealing in Explorer is best-effort; ignore.
    }
  }

  selectTask(id: Id | null): void {
    this.selectedTask.value = id;
    void this.loadDetail();
  }
}

function sameId(a: Id | null, b: Id): boolean {
  return a != null && String(a) === String(b);
}

function baseName(path: string): string {
  const norm = path.replace(/[\\/]+$/, "");
  const idx = Math.max(norm.lastIndexOf("\\"), norm.lastIndexOf("/"));
  return idx >= 0 ? norm.slice(idx + 1) : norm;
}

function parentDir(path: string): string {
  const norm = path.replace(/[\\/]+$/, "");
  const idx = Math.max(norm.lastIndexOf("\\"), norm.lastIndexOf("/"));
  return idx > 0 ? norm.slice(0, idx) : "";
}

export const store = new WorkspaceStore();
