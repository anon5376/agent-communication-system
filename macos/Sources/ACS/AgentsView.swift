import AppKit
import SwiftUI
import ACSCore

struct AgentsView: View {
    @EnvironmentObject private var store: WorkspaceStore
    @State private var starting: AgentRecord?
    @State private var stopping: AgentRecord?
    @State private var confirmSetup = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                HStack(alignment: .top) {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Your agents").font(.largeTitle.weight(.semibold))
                        Text("Independent tools. Shared tasks and history.").font(.title3).foregroundStyle(.secondary)
                    }
                    Spacer()
                    Button("Find Installed CLIs") { Task { await store.detect() } }.disabled(store.busy)
                }
                if store.agents.isEmpty {
                    VStack(alignment: .leading, spacing: 14) {
                        Text("Start with the tools you already use.").font(.title2.weight(.medium))
                        Text("ACS can detect supported coding CLIs on this Mac and create a local crew configuration. Nothing starts automatically. Sign in to each provider separately using its own CLI.")
                            .foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                        Button("Set Up Local Crew…") { confirmSetup = true }
                            .buttonStyle(.borderedProminent).disabled(!store.canWrite || store.snapshot?.simulated == true)
                    }.padding(.vertical, 20)
                } else {
                    let simulated = store.snapshot?.simulated == true
                    VStack(spacing: 0) {
                        ForEach(store.agents) { agent in
                            HStack(spacing: 14) {
                                Text(agent.harness.prefix(1).uppercased())
                                    .font(.callout.weight(.semibold)).foregroundStyle(.indigo)
                                    .frame(width: 34, height: 34)
                                    .background(.indigo.opacity(0.12), in: RoundedRectangle(cornerRadius: 8))
                                VStack(alignment: .leading, spacing: 5) {
                                    Text(agent.id).font(.headline)
                                    Text("\(agent.role) · \(agent.harness) · \(agent.model)").font(.caption).foregroundStyle(.secondary)
                                }
                                Spacer(minLength: 12)
                                VStack(alignment: .leading, spacing: 3) {
                                    HStack(spacing: 6) {
                                        Group {
                                            if agent.paused {
                                                Circle().fill(.orange)
                                            } else if agent.running {
                                                Circle().fill(.green)
                                            } else {
                                                Circle().strokeBorder(Color.secondary.opacity(0.8), lineWidth: 1.5)
                                            }
                                        }
                                        .frame(width: 8, height: 8)
                                        Text(agent.paused ? "Paused" : agent.running ? "Running" : "Not running")
                                            .font(.callout.weight(.semibold))
                                    }
                                    Text(agent.lastSeenMs.map { "Seen \(relativeTime($0))" } ?? "Not seen on the bus yet")
                                        .font(.caption).foregroundStyle(.secondary)
                                }
                                .frame(width: 190, alignment: .leading)
                                VStack(alignment: .trailing, spacing: 4) {
                                    if agent.running {
                                        HStack {
                                            Button(agent.paused ? "Resume" : "Pause") {
                                                Task { await store.mutate(agent.paused ? "resume" : "pause", ["id": .string(agent.id)]) }
                                            }
                                            Button("Stop…", role: .destructive) { stopping = agent }
                                        }
                                    } else {
                                        Button("Start…") { starting = agent }
                                            .disabled(!store.canWrite || simulated)
                                        if simulated {
                                            Text("Sample agents can’t start")
                                                .font(.caption).foregroundStyle(.secondary)
                                        }
                                    }
                                }
                                .frame(width: 170, alignment: .trailing)
                            }.padding(.vertical, 16)
                            Divider()
                        }
                    }
                    Text("“Running” means ACS is managing that agent’s process. It doesn’t prove the provider is signed in or making progress. “Seen” is the agent’s last activity on the bus, which can also come from a CLI started elsewhere.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                if !store.providers.isEmpty {
                    Divider()
                    Text("CLI detection").font(.title2.weight(.semibold))
                    Text("Installed is not authenticated. These checks do not make model calls.").foregroundStyle(.secondary)
                    ForEach(store.providers) { provider in
                        HStack(alignment: .top, spacing: 16) {
                            Image(systemName: provider.path == nil ? "minus.circle" : "checkmark.circle").foregroundStyle(.secondary)
                            VStack(alignment: .leading, spacing: 5) {
                                HStack {
                                    Text(provider.name).font(.headline)
                                    Spacer()
                                    Text(provider.path == nil ? "Not installed" : "Installed · authentication unverified").font(.caption).foregroundStyle(.secondary)
                                }
                                if let path = provider.path { Text(path).font(.caption.monospaced()).foregroundStyle(.secondary).textSelection(.enabled) }
                                if !provider.signIn.isEmpty { Text(provider.signIn).font(.callout).textSelection(.enabled) }
                                if provider.requiresApproval { Text("This integration requires explicit approval in crew configuration before starting.").font(.caption).foregroundStyle(.orange) }
                            }
                        }.padding(.vertical, 6)
                    }
                }
                if !store.agents.isEmpty && store.snapshot?.simulated != true {
                    Button("Set Up Missing Crew Configuration…") { confirmSetup = true }.disabled(!store.canWrite)
                }
            }.padding(32).frame(maxWidth: 1000, alignment: .leading).frame(maxWidth: .infinity, alignment: .leading)
        }
        .sheet(item: $starting) { StartAgentSheet(agent: $0).environmentObject(store) }
        .alert("Set up a local crew?", isPresented: $confirmSetup) {
            Button("Cancel", role: .cancel) {}
            Button("Set Up") { Task { await store.mutate("setup"); await store.detect() } }
        } message: {
            Text("ACS will detect installed CLIs and create missing crew files and agent identities in this workspace. Existing custom configuration is preserved. No providers will run.")
        }
        .alert("Stop this agent?", isPresented: Binding(get: { stopping != nil }, set: { if !$0 { stopping = nil } })) {
            Button("Keep Running", role: .cancel) { stopping = nil }
            Button("Stop Agent", role: .destructive) {
                if let id = stopping?.id { Task { await store.mutate("stop", ["ids": .array([.string(id)])]) } }
                stopping = nil
            }
        } message: { Text("This stops its managed process. Task history remains; review any claimed work before returning it to the queue.") }
    }
}

struct StartAgentSheet: View {
    @EnvironmentObject private var store: WorkspaceStore
    @Environment(\.dismiss) private var dismiss
    let agent: AgentRecord
    @State private var folder: URL?
    @State private var approved = false
    @State private var failure: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            Text("Start \(agent.id)").font(.title2.weight(.semibold))
            Text("This starts a real coding agent.").font(.headline)
            Text("It may send project content to its provider, modify files, run commands, and incur charges under your provider account. ACS coordination controls are not a sandbox or a guaranteed dollar cap.")
                .foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: 8) {
                Text("Project folder").font(.headline)
                HStack {
                    Text(folder?.path ?? "Choose a project folder").font(.callout.monospaced()).lineLimit(3).textSelection(.enabled)
                    Spacer()
                    Button("Choose…", action: chooseFolder).disabled(store.busy)
                }
            }
            Toggle("I trust this project and approve this agent running in it.", isOn: $approved)
            Text("The agent keeps running if you close ACS. Use Stop in Agents to stop its supervisor.")
                .font(.caption).foregroundStyle(.secondary)
            if let failure { Text(failure).foregroundStyle(.red).textSelection(.enabled) }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction).disabled(store.busy)
                Button("Start Agent") { Task { await start() } }
                    .buttonStyle(.borderedProminent).disabled(!approved || folder == nil || !store.canWrite)
            }
        }.padding(24).frame(width: 580).interactiveDismissDisabled(store.busy)
            .onAppear { folder = store.project }
    }

    private func chooseFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.prompt = "Choose Project"
        if panel.runModal() == .OK { folder = panel.url; approved = false }
    }

    private func start() async {
        guard let folder else { return }
        if await store.mutate("start", ["ids": .array([.string(agent.id)]), "workdir": .string(folder.path), "confirmed": .bool(approved)]) {
            dismiss()
        } else { failure = store.error ?? "The agent could not be started." }
    }
}
