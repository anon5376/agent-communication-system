# ACS for Windows

The ACS desktop app for Windows — the same design as the macOS app
(`macos/DESIGN.md`), built on Tauri v2 (WebView2) over the same
`acs-desktop` JSON bridge. It is a companion to the terminal tools, never a
replacement: tasks, review decisions, agents, and messages on the same
`bus.db`, with the same simulated sample workspace for a no-risk look around.

## What it is

- **Welcome** — "Different agents. One place to work." Open a project folder,
  try the simulated sample, or connect an existing `bus.db`.
- **Tasks** — the full task list with search and "Show completed", plus a
  per-task inspector: brief, acceptance criteria, submitted work, changed
  files, worker-reported checks, review, notes, and related messages.
- **Review decisions** — accept, request changes, return to queue, or cancel,
  each from a sheet that requires a note, exactly like the macOS app.
- **Agents** — the crew on this bus; starting a supervisor requires an
  explicit per-agent confirmation with the trusted-folder checkbox and the
  verbatim provider warning. Detection reports installed CLIs without claiming
  they are signed in.
- **Messages** — bus traffic to and from the operator, with a compose sheet.
  Messages may wake an already running agent; they do not start stopped ones.
- **Read-only buses** — when the operator identity is unavailable the app
  shows the whole workspace but disables every mutation.

Nothing invented: no progress percentages, no cost claims, no sign-in claims
beyond what `acs-desktop detect` reports.

## Architecture

```
windows/
├── index.html / src/            Preact + TypeScript frontend (Vite)
│   ├── model.ts                 bus records + state copy (mirrors ACSApp.swift)
│   ├── client.ts                acs_request + workspace path commands
│   ├── store.ts                 WorkspaceStore (connect/refresh/mutate)
│   └── views/                   Welcome, Tasks+Inspector, Sheets, Agents, Messages
└── src-tauri/                   Rust shell
    ├── src/helper.rs            spawn acs-desktop.exe, pipe transport
    ├── src/lib.rs               Tauri commands
    ├── src/bin/fake-acs-desktop.rs  protocol-identical dev double
    └── tests/transport.rs       fake-helper transport test set
```

The only Tauri command that talks to the bus is
`acs_request(action, payload)`: it writes one
`{"version":1,"dbPath":…,"action":…,"payload":…}` object to the helper's
stdin and reads exactly one `{ok,data}` / `{ok,error:{code,message}}` envelope
from stdout (stderr is diagnostics only). Per `protocol/desktop-v1.md` it:

- spawns the bundled `acs-desktop.exe` directly — no shell,
  `CREATE_NO_WINDOW`;
- waits on the **child handle**, never on pipe EOF, so a descendant that
  inherited the helper's pipes cannot stall a request;
- applies a 30 s timeout (kill on expiry) and a 16 MiB output bound;
- rejects an `ok:true` reply from a nonzero exit;
- passes `data` through as raw JSON with serde_json arbitrary precision, and
  the frontend parses with lossless-json — i64 task ids keep every digit
  past 2^53.

### The helper on this branch

The real `acs-desktop.exe` is the Rust bridge in `rust/` — the same binary
the macOS app shells out to. A separate effort is porting that crate to
Windows; until it lands, `windows/scripts/build-installer.ps1` falls back to
`src/bin/fake-acs-desktop.rs`, a protocol-v1 double that serves a JSON-file
bus seeded with the same fixture `aos demo` writes. The app cannot tell the
difference, and once the real Windows helper builds, the same script bundles
it (plus `qagent.exe` and `aos.exe`) with no app changes.

## Building

Requires Node 20+, a stable Rust toolchain, MSVC build tools, and the
WebView2 runtime (to *run* the app; the installer only needs it at launch).

```powershell
# from the repository root — one command, everything included
powershell -ExecutionPolicy Bypass -File windows\scripts\build-installer.ps1
```

Output: `windows/src-tauri/target/release/bundle/nsis/ACS-<version>-x64-setup.exe`
plus a `.sha256` checksum. Per-user install into `%LOCALAPPDATA%` with a
Start menu shortcut.

For development:

```bash
cd windows
npm install
npm run typecheck   # tsc --noEmit
npm run lint        # eslint
npm run test        # vitest (model/transport copy tests)
cd src-tauri && cargo test   # fake-helper transport tests
npm run tauri dev   # hot-reload dev run (needs the fake staged, see script)
```

## Signing

The installer is **not Authenticode-signed**. SmartScreen shows a warning on
downloaded copies; that is expected for this preview. (We intentionally do
not document ways around SmartScreen.)

## Workspace storage

Buses live under `%APPDATA%\ACS\` (the same relative spot as macOS's
`~/Library/Application Support/ACS`; the app's own binaries sit in
`%LOCALAPPDATA%\ACS` so per-user install and data never share a folder):

- `Workspaces\<sha256 of the canonical project path>\bus.db` — one per
  project opened with "Open a Project";
- `Samples\<uuid>\bus.db` — each "Try the Sample" gets a fresh simulated bus.

The app remembers the last workspace and reopens it on launch.

## Keyboard shortcuts

- `Ctrl+N` new task · `Ctrl+O` open a project · `Ctrl+R` refresh
- `Esc` closes sheets; every control is keyboard reachable and labelled.
