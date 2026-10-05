# ACS for macOS (SwiftUI)

Native macOS front end for the agent-communication-system. The SwiftUI app is a
thin shell over the existing Rust bus: the Rust helper binaries ship inside the
app bundle and the app talks to them locally.

## First run

Open the DMG and drag **ACS** into **Applications**. The app requires macOS 14
or newer and a build matching your Mac's processor.

- **Try the Sample** opens an isolated, clearly labelled simulated workspace.
  Explore Tasks, Needs review, Agents, and Messages without accounts or model calls.
- **Open a Project** creates an empty local workspace for a folder you choose.
  It does not modify that project's files or start agents.
- **Connect an existing bus** opens a trusted ACS `bus.db`, including its normal
  additive migrations. Existing operator credentials are required to make changes.

For real work, open **Agents → Find Installed CLIs → Set Up Local Crew**, then explicitly start
the agents you want. Installed is not authenticated: follow the displayed sign-in
guidance in the provider's own CLI. Starting an agent requires choosing a project
folder and accepting that it may run commands, modify files, send project content
to its provider, and incur charges. ACS is not a sandbox or a guaranteed spend cap.

Use **New Task** to write a brief, acceptance criteria, and file scope. Choose a
separate reviewer or review as the operator. Creating a task does not start a
stopped agent. Open **Needs review** to inspect submitted work before accepting it
or requesting changes; worker-reported checks are not independently verified by ACS.
Return open/claimed tasks to the queue to release ownership without losing their
requirements. Failed/closed tasks are not silently cloned into weaker replacements.

Workspace databases live in `~/Library/Application Support/ACS`. The workspace
menu can reveal the selected database in Finder. Existing terminal commands remain
available; this app adds a native operator surface rather than replacing them.

## Layout

```
macos/
  Package.swift          SwiftPM package (tools 5.9, macOS 14+)
  Resources/
    Info.plist           bundle metadata copied into ACS.app
  Sources/
    ACSCore/             library target: model + transport layer
    ACS/                 executable target: SwiftUI app (depends on ACSCore)
  Tests/
    ACSCoreTests/        unit tests for ACSCore
  scripts/
    build-dmg.sh         build + assemble + sign + package
    make-icon.swift      procedural AppKit icon generator (optional)
```

## Build requirements

- macOS 14+ host, Xcode/Swift toolchain (`swift --version`)
- Rust stable toolchain (`cargo`)

## Building the app and DMG

```sh
macos/scripts/build-dmg.sh
```

The script:

1. Builds the Rust helpers with `cargo build --release --locked`.
2. Builds the SwiftUI executable with `swift build -c release`.
3. Assembles `ACS.app`:

   ```
   ACS.app/Contents/MacOS/ACS            SwiftUI executable
   ACS.app/Contents/Helpers/acs-desktop  desktop bridge (app locates this exact path)
   ACS.app/Contents/Helpers/qagent       bus/CLI agent (acs-desktop finds it as a sibling)
   ACS.app/Contents/Helpers/aos          harness
   ACS.app/Contents/Resources/AppIcon.icns (when make-icon.swift is present)
   ACS.app/Contents/Info.plist
   ```

4. Ad-hoc code-signs nested helpers first, then the bundle, and verifies with
   `codesign --verify --deep --strict`. Set `ACS_CODESIGN_IDENTITY` to a
   Developer ID Application identity to sign for distribution instead.
5. Creates `macos/dist/ACS-<arch>.dmg` (UDZO) containing the app and an
   `/Applications` symlink, plus `ACS-<arch>.dmg.sha256`, then verifies the
   image by attaching, listing, and detaching it.

Builds are native to the host architecture (`arm64` or `x86_64`) — the DMG name
carries the arch and no universal build is claimed.

## Signing and Gatekeeper

Default output is **ad-hoc signed and not notarized**. On first launch macOS
will warn that the app is from an unidentified developer; open it via
right-click → Open, or approve it in System Settings → Privacy & Security. For
distribution outside trusted machines, configure a Developer ID identity via
`ACS_CODESIGN_IDENTITY` and notarize separately.

## CI

`.github/workflows/swiftui-macos.yml` runs on pushes/PRs to `swiftui` that
touch `macos/` or `rust/`: package tests, DMG build, and artifact upload
(DMG + checksum). It does not publish releases.
