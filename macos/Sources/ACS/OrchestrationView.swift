import SwiftUI
import ACSCore

struct GoalsView: View {
    @EnvironmentObject private var store: WorkspaceStore
    @State private var goal = ""
    @State private var mission = "run"
    @State private var assignee = ""
    @State private var showBrief = false

    private var preset: MissionPrompt? { store.orchestration?.missions.first { $0.name == mission } }
    private var eligibleAgents: [AgentRecord] {
        store.agents.filter { agent in
            store.orchestration?.crew.contains { $0.id == agent.id && !$0.enabled } != true
        }
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                Text("What should the crew do?").font(.largeTitle.weight(.semibold))
                Text("Write the outcome. Choose a preset to add a brief and acceptance criteria.")
                    .foregroundStyle(.secondary)
                Text("Goal").font(.headline)
                TextEditor(text: $goal).frame(minHeight: 90).border(.quaternary).accessibilityLabel("Goal")
                HStack {
                    Picker("Preset", selection: $mission) {
                        ForEach(store.orchestration?.missions ?? []) { Text($0.name).tag($0.name) }
                    }
                    Picker("Hand to", selection: $assignee) {
                        Text(store.orchestration?.goalOwner.map { "\($0) (crew lead)" } ?? "Any eligible agent").tag("")
                        ForEach(eligibleAgents) { Text($0.id).tag($0.id) }
                    }
                }
                if let preset {
                    Text(preset.summary).foregroundStyle(.secondary)
                    DisclosureGroup("Preview the brief", isExpanded: $showBrief) {
                        VStack(alignment: .leading, spacing: 12) {
                            Text(preset.brief.replacingOccurrences(of: "{goal}", with: goal)).textSelection(.enabled)
                            Text("Done when").font(.headline)
                            Text(preset.acceptance.replacingOccurrences(of: "{goal}", with: goal)).textSelection(.enabled)
                        }.frame(maxWidth: .infinity, alignment: .leading).padding(.vertical, 12)
                    }
                }
                HStack {
                    Text("Queuing a goal does not start a provider. Start agents in Agents.")
                        .font(.caption).foregroundStyle(.secondary)
                    Spacer()
                    Button("Queue goal") { Task { await queue() } }
                        .buttonStyle(.borderedProminent)
                        .disabled(!store.canWrite || preset == nil || goal.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || store.orchestration?.crewError != nil)
                }
                Divider()
                Text("Recent goals").font(.title2.weight(.semibold))
                if store.orchestration?.goals.isEmpty == true { Text("No goals queued yet.").foregroundStyle(.secondary) }
                ForEach(store.orchestration?.goals ?? []) { item in
                    Button {
                        store.mode = .communication
                        store.destination = .tasks
                        store.selectedTask = item.id
                    } label: {
                        HStack {
                            VStack(alignment: .leading, spacing: 6) {
                                Text(item.title).font(.headline)
                                Text("\(taskStateTitle(item.state)) · \(item.assignee ?? "Unassigned")").font(.caption).foregroundStyle(.secondary)
                            }
                            Spacer()
                            Text("#\(item.id)").monospacedDigit()
                            Image(systemName: "arrow.right")
                        }.padding(.vertical, 8).contentShape(Rectangle())
                    }.buttonStyle(.plain)
                    Divider()
                }
            }.padding(32).frame(maxWidth: 1000, alignment: .leading).frame(maxWidth: .infinity, alignment: .leading)
        }
        .onChange(of: store.orchestration?.crew.filter { !$0.enabled }.map(\.id)) { _, disabled in
            if disabled?.contains(assignee) == true { assignee = "" }
        }
    }

    private func queue() async {
        var payload: [String: JSONValue] = ["goal": .string(goal), "mission": .string(mission)]
        if !assignee.isEmpty { payload["to"] = .string(assignee) }
        if let project = store.project { payload["project"] = .string(project.path) }
        if await store.mutate("startGoal", payload) { goal = "" }
    }
}

private struct PromptDraft: Identifiable {
    let id = UUID()
    let role: Bool
    let name: String
    let text: String
}

struct PresetsView: View {
    @EnvironmentObject private var store: WorkspaceStore
    @State private var roles = false
    @State private var draft: PromptDraft?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                Text("Prompt presets").font(.largeTitle.weight(.semibold))
                Text("Goal templates and the instructions agents read before each turn.").foregroundStyle(.secondary)
                HStack {
                    Picker("Prompt type", selection: $roles) {
                        Text("Goal presets").tag(false)
                        Text("Agent roles").tag(true)
                    }.pickerStyle(.segmented).frame(maxWidth: 320)
                    Spacer()
                    Button("New preset") { draft = PromptDraft(role: roles, name: "", text: "") }.disabled(!store.canWrite)
                }
                if roles {
                    ForEach(store.orchestration?.roles ?? []) { prompt in
                        promptRow(prompt.name, summary: prompt.custom ? "Edited role prompt" : "Built-in role prompt", text: prompt.text)
                    }
                } else {
                    ForEach(store.orchestration?.missions ?? []) { prompt in
                        promptRow(prompt.name, summary: prompt.summary, text: prompt.text)
                    }
                }
            }.padding(32).frame(maxWidth: 1000, alignment: .leading).frame(maxWidth: .infinity, alignment: .leading)
        }
        .sheet(item: $draft) { PromptEditor(draft: $0).environmentObject(store) }
        .onAppear { openRole() }
        .onChange(of: store.presetRole) { _, _ in openRole() }
    }

    private func promptRow(_ name: String, summary: String, text: String) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                VStack(alignment: .leading, spacing: 6) {
                    Text(name).font(.headline)
                    Text(summary).foregroundStyle(.secondary)
                }
                Spacer()
                Button(store.canWrite ? "Edit" : "Read") { draft = PromptDraft(role: roles, name: name, text: text) }
            }.padding(.vertical, 8)
            Divider()
        }
    }

    private func openRole() {
        guard let name = store.presetRole else { return }
        store.presetRole = nil
        guard let role = store.orchestration?.roles.first(where: { $0.name == name }) else {
            store.error = "That role prompt is not available. Edit the configured instructions file directly."
            return
        }
        roles = true
        draft = PromptDraft(role: true, name: name, text: role.text)
    }
}

private struct PromptEditor: View {
    @EnvironmentObject private var store: WorkspaceStore
    @Environment(\.dismiss) private var dismiss
    let draft: PromptDraft
    @State private var name: String
    @State private var text: String
    @State private var failure: String?

    init(draft: PromptDraft) {
        self.draft = draft
        _name = State(initialValue: draft.name)
        _text = State(initialValue: draft.text)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text(draft.role ? "Agent role prompt" : "Goal preset").font(.title2.weight(.semibold))
            TextField("Name", text: $name).disabled(!draft.name.isEmpty || !store.canWrite)
            Text("Names use 1–40 letters, digits, hyphens or underscores. Goal presets use ## brief and ## acceptance sections; {goal} inserts your goal.")
                .font(.caption).foregroundStyle(.secondary)
            TextEditor(text: $text).font(.system(.body, design: .monospaced)).frame(minHeight: 300)
                .border(.quaternary).accessibilityLabel("Prompt text").disabled(!store.canWrite)
            if let failure { Text(failure).foregroundStyle(.red) }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction).disabled(store.busy)
                Button("Save prompt") {
                    Task {
                        if await store.mutate(draft.role ? "saveRole" : "saveMission", ["name": .string(name), "text": .string(text)]) {
                            dismiss()
                        } else { failure = store.error }
                    }
                }.buttonStyle(.borderedProminent).disabled(!store.canWrite || name.isEmpty || text.isEmpty)
            }
        }.padding(24).frame(width: 680).interactiveDismissDisabled(store.busy)
    }
}

struct CrewConfiguration: View {
    @EnvironmentObject private var store: WorkspaceStore
    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("Crew configuration").font(.title2.weight(.semibold))
            Text("Disable a member to stop it and prevent new starts. Enabling it does not start a provider.")
                .foregroundStyle(.secondary)
            ForEach(store.orchestration?.crew ?? []) { member in
                CrewConfigurationRow(member: member)
                Divider()
            }
            if store.orchestration?.configured == false {
                Text(store.snapshot?.simulated == true
                    ? "Sample agents can't be configured. Open a project to connect your own crew."
                    : "No crew configured. Use Set Up Local Crew below to connect installed CLIs.")
                    .foregroundStyle(.secondary)
            }
        }
    }
}

private struct CrewConfigurationRow: View {
    @EnvironmentObject private var store: WorkspaceStore
    let member: CrewMember
    @State private var description = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                VStack(alignment: .leading, spacing: 4) {
                    Text(member.id).font(.headline)
                    Text("\(member.role) · \(member.cli) · \(member.running ? "Running" : "Not running")").font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
                Toggle("Enabled", isOn: Binding(get: { member.enabled }, set: { enabled in
                    Task { await store.mutate("setAgent", ["id": .string(member.id), "enabled": .bool(enabled)]) }
                })).toggleStyle(.switch).disabled(!store.canWrite)
            }
            HStack {
                TextField("Description", text: $description).disabled(!store.canWrite)
                Button("Save description") {
                    Task { await store.mutate("setAgent", ["id": .string(member.id), "description": .string(description)]) }
                }.disabled(!store.canWrite || description == member.description)
            }
            if let role = member.editableRole, store.orchestration?.roles.contains(where: { $0.name == role }) == true {
                Button("Edit role prompt") { store.presetRole = role; store.destination = .presets }
            } else {
                Text("Instructions: \(member.instructions ?? "none configured"). Edit custom file paths outside the app.")
                    .font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
            }
        }.onAppear { description = member.description }
    }
}
