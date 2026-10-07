// Thin client over the Tauri backend, mirroring ACSClient.swift. The backend
// owns the acs-desktop child process: we send an action + payload and get back
// the helper's `data` as raw JSON text, parsed with lossless-json so i64 task
// ids keep full precision end to end.

import { invoke } from "@tauri-apps/api/core";
import { parse as parseLossless, stringify as stringifyLossless } from "lossless-json";

export class AcsError extends Error {
  readonly code: string;

  constructor(code: string, message: string) {
    super(message);
    this.name = "AcsError";
    this.code = code;
  }
}

interface BackendError {
  kind?: string;
  code?: string;
  message?: string;
}

function toAcsError(raw: unknown): AcsError {
  if (raw instanceof AcsError) return raw;
  if (typeof raw === "object" && raw !== null) {
    const err = raw as BackendError;
    if (typeof err.message === "string" && err.message) {
      return new AcsError(err.code ?? err.kind ?? "helper_error", err.message);
    }
  }
  if (typeof raw === "string" && raw) return new AcsError("helper_error", raw);
  return new AcsError("helper_error", "The ACS helper request failed.");
}

/**
 * Send one action to the bundled acs-desktop helper through the backend.
 * `payload` is lossless-stringified so integer ids survive the boundary
 * untouched; the reply arrives as raw JSON text and is parsed losslessly.
 */
export async function acsRequest<T>(
  dbPath: string,
  action: string,
  payload: Record<string, unknown> = {},
): Promise<T> {
  const payloadJson = stringifyLossless(payload) ?? "{}";
  try {
    const reply = await invoke<string>("acs_request", {
      dbPath,
      action,
      payloadJson,
    });
    return parseLossless(reply) as T;
  } catch (raw) {
    throw toAcsError(raw);
  }
}

/** The app's ACS support directory (%APPDATA%\ACS). */
export async function acsBaseDir(): Promise<string> {
  return invoke<string>("acs_base_dir");
}

/** Deterministic bus.db path for a project folder (sha256 of canonical path). */
export async function workspaceDbPath(projectPath: string): Promise<string> {
  return invoke<string>("acs_workspace_db_path", { projectPath });
}

/** Fresh sample bus.db path under Samples/<uuid>/. */
export async function sampleDbPath(): Promise<string> {
  return invoke<string>("acs_sample_db_path");
}

/** "real" or "fake" — which helper the installer bundled (build flavor). */
export async function acsBuildFlavor(): Promise<string> {
  return invoke<string>("acs_build_flavor");
}

/** Reveal the bus database in Explorer (selects the file). */
export async function revealDatabase(dbPath: string): Promise<void> {
  await invoke("acs_reveal", { path: dbPath });
}
