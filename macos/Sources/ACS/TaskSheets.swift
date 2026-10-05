import SwiftUI
import ACSCore

struct NewTaskSheet: View {
    @EnvironmentObject private var store: WorkspaceStore
    @Environment(\.dismiss) private var dismiss
    @State private var title = ""
    @State private var brief = ""
    @State private var acceptance = ""
    @State private var assignee = ""
    @State private var reviewer = "operator"
    @State private var scope = ""
    @State private var priority = "normal"
    @State private var failure: String?

    private var valid: Bool {
        !title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty &&
        !brief.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty &&
        !acceptance.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty &&
        (assignee.isEmpty || reviewer != assignee) && title.count <= 500
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("Give your agents a clear task").font(.title2.weight(.semibold))
            Text("Describe the outcome. Define what good looks like. Keep review independent.")
                .foregroundStyle(.secondary)
            Form {
                TextField("Task title", text: $title, prompt: Text("e.g. Add validation to the signup form"))
                LabeledContent("Brief") { TextEditor(text: $brief).frame(height: 100).border(.quaternary).accessibilityLabel("Task brief") }
                LabeledContent("Acceptance criteria") { TextEditor(text: $acceptance).frame(height: 80).border(.quaternary).accessibilityLabel("Acceptance criteria") }
                Picker("Worker", selection: $assignee) {
                    Text("Any eligible worker").tag("")
                    ForEach(store.agents) { Text($0.id).tag($0.id) }
                }
                Picker("Reviewer", selection: $reviewer) {
                    Text("Me (operator)").tag("operator")
                    ForEach(store.agents.filter { $0.id != assignee }) { Text($0.id).tag($0.id) }
                }
                DisclosureGroup("Scope and priority") {
                    TextField("File scope", text: $scope, prompt: Text("src/forms/, tests/ (comma separated)"))
                    Text("A scope is task guidance, not an operating-system sandbox.").font(.caption).foregroundStyle(.secondary)
                    Picker("Priority", selection: $priority) {
                        ForEach(["low", "normal", "high", "urgent"], id: \.self) { Text($0.capitalized).tag($0) }
                    }
                }
            }.formStyle(.grouped)
            if let failure { Text(failure).foregroundStyle(.red).font(.callout).textSelection(.enabled) }
            HStack {
                Text("Creating a task doesn’t start an agent.").font(.caption).foregroundStyle(.secondary)
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction).disabled(store.busy)
                Button("Create Task") { Task { await create() } }
                    .buttonStyle(.borderedProminent).keyboardShortcut(.defaultAction)
                    .disabled(!valid || !store.canWrite)
            }
        }.padding(24).frame(width: 640).interactiveDismissDisabled(store.busy)
            .onChange(of: assignee) { _, next in if reviewer == next { reviewer = "operator" } }
    }

    private func create() async {
        let paths = scope.split(separator: ",").map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }.filter { !$0.isEmpty }
        let payload: [String: JSONValue] = [
            "title": .string(title), "brief": .string(brief), "acceptance": .string(acceptance),
            "to": assignee.isEmpty ? .null : .string(assignee), "reviewer": .string(reviewer),
            "priority": .string(priority), "pathScopes": .array(paths.map(JSONValue.string)),
            "project": store.project.map { .string($0.path) } ?? .null
        ]
        if await store.mutate("createTask", payload) { dismiss() } else { failure = store.error ?? "The task could not be created." }
    }
}

enum TaskDecision: String, Identifiable {
    case accept, changes, requeue, cancel
    var id: String { rawValue }
    var title: String {
        switch self {
        case .accept: return "Accept this work?"
        case .changes: return "What needs to change?"
        case .requeue: return "Return this task to the queue?"
        case .cancel: return "Cancel this task?"
        }
    }
    var button: String {
        switch self {
        case .accept: return "Accept Work"
        case .changes: return "Request Changes"
        case .requeue: return "Return to Queue"
        case .cancel: return "Cancel Task"
        }
    }
    var explanation: String {
        switch self {
        case .accept: return "Confirm that you reviewed the submission against its acceptance criteria. This marks the task accepted."
        case .changes: return "Give actionable feedback. The worker can revise and submit again."
        case .requeue: return "This releases the current claim and preserves the task’s requirements. A running provider may still be working; stop it first if needed."
        case .cancel: return "This closes the task but preserves its history. It does not terminate an already running provider."
        }
    }
}

struct DecisionSheet: View {
    @EnvironmentObject private var store: WorkspaceStore
    @Environment(\.dismiss) private var dismiss
    let task: TaskRecord
    let decision: TaskDecision
    @State private var feedback = ""
    @State private var failure: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text(decision.title).font(.title2.weight(.semibold))
            Text("#\(task.id) · \(task.title)").font(.headline)
            Text(decision.explanation).foregroundStyle(.secondary)
            Text(decision == .accept ? "Review note (optional)" : "Feedback / reason").font(.headline)
            TextEditor(text: $feedback).frame(height: 130).border(.quaternary).accessibilityLabel("Feedback or reason")
            if let failure { Text(failure).foregroundStyle(.red).textSelection(.enabled) }
            HStack {
                Spacer()
                Button("Go Back") { dismiss() }.keyboardShortcut(.cancelAction).disabled(store.busy)
                Button(decision.button, role: decision == .cancel ? .destructive : nil) { Task { await submit() } }
                    .buttonStyle(.borderedProminent)
                    .disabled(!store.canWrite || (decision != .accept && feedback.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty))
            }
        }.padding(24).frame(width: 540).interactiveDismissDisabled(store.busy)
    }

    private func submit() async {
        let action = (decision == .accept || decision == .changes) ? "reviewTask" : decision.rawValue
        var payload: [String: JSONValue] = ["id": .number(Double(task.id))]
        if action == "reviewTask" {
            payload["accept"] = .bool(decision == .accept)
            payload["feedback"] = .string(feedback)
        } else { payload["reason"] = .string(feedback) }
        if await store.mutate(action, payload) { dismiss() } else { failure = store.error ?? "The task could not be updated." }
    }
}
