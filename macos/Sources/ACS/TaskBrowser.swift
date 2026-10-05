import SwiftUI
import ACSCore

struct TaskBrowser: View {
    @EnvironmentObject private var store: WorkspaceStore
    let reviewOnly: Bool
    @State private var query = ""
    @State private var includeClosed = false

    private var filtered: [TaskRecord] {
        store.tasks.filter {
            (reviewOnly ? $0.state == "submitted" : (includeClosed || !$0.isClosed)) &&
            (query.isEmpty || "\($0.id) \($0.title) \($0.assignee ?? "")".localizedCaseInsensitiveContains(query))
        }.sorted {
            let first = $0.state == "submitted", second = $1.state == "submitted"
            return first != second ? first : $0.updatedMs > $1.updatedMs
        }
    }

    var body: some View {
        HSplitView {
            VStack(alignment: .leading, spacing: 0) {
                VStack(alignment: .leading, spacing: 12) {
                    HStack {
                        Text(reviewOnly ? "Needs review" : "Tasks").font(.title2.weight(.semibold))
                        Spacer()
                        Text("\(filtered.count)").foregroundStyle(.secondary).monospacedDigit()
                    }
                    TextField("Search tasks", text: $query).textFieldStyle(.roundedBorder)
                    if !reviewOnly { Toggle("Show completed tasks", isOn: $includeClosed).font(.caption) }
                }.padding(18)
                Divider()
                if filtered.isEmpty {
                    EmptyState(symbol: reviewOnly ? "tray" : "checklist", title: query.isEmpty ? (reviewOnly ? "All caught up" : "No tasks yet") : "No matches", message: reviewOnly ? "Submitted work will appear here, ready for a decision." : "Create a task with a clear goal and acceptance criteria.")
                } else {
                    List(filtered, selection: $store.selectedTask) { task in
                        HStack(alignment: .top, spacing: 10) {
                            Image(systemName: task.stateSymbol).foregroundStyle(task.stateColor).frame(width: 17).padding(.top, 3)
                            VStack(alignment: .leading, spacing: 7) {
                                Text(task.title).font(.headline).lineLimit(3)
                                HStack {
                                    Text(task.stateTitle)
                                    Spacer()
                                    Text("#\(task.id)").monospacedDigit()
                                }.font(.caption).foregroundStyle(.secondary)
                                Text(task.assignee ?? "Unassigned").font(.caption).foregroundStyle(.secondary)
                            }
                        }.padding(.vertical, 8).tag(task.id)
                    }.listStyle(.inset)
                }
                if store.snapshot?.truncated == true {
                    Text("Showing the newest 1,000 tasks. Full history remains in the bus.")
                        .font(.caption).foregroundStyle(.secondary).padding(12)
                }
            }.frame(minWidth: 270, idealWidth: 330, maxWidth: 390)
            Group {
                if store.detailLoading && store.detail?.task.id != store.selectedTask {
                    ProgressView("Loading task…").frame(maxWidth: .infinity, maxHeight: .infinity)
                } else if let detail = store.detail, detail.task.id == store.selectedTask {
                    TaskInspector(detail: detail)
                } else if let error = store.detailError {
                    EmptyState(symbol: "exclamationmark.triangle", title: "Couldn’t load this task", message: error)
                } else {
                    EmptyState(symbol: "sidebar.right", title: "The whole task, in one place", message: "Select a task to see its brief, owner, submission, and review history.")
                }
            }.frame(minWidth: 400, maxWidth: .infinity)
        }
        .task(id: store.selectedTask) { await store.loadDetail() }
    }
}

struct TaskInspector: View {
    @EnvironmentObject private var store: WorkspaceStore
    let detail: TaskDetail
    @State private var decision: TaskDecision?
    private var task: TaskRecord { detail.task }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                VStack(alignment: .leading, spacing: 12) {
                    HStack {
                        Text("TASK #\(task.id)").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                        Spacer()
                        if task.priority != "normal" { Text(task.priority.capitalized + " priority").font(.caption) }
                    }
                    Text(task.title).font(.system(size: 25, weight: .semibold)).textSelection(.enabled)
                    Label(task.stateTitle, systemImage: task.stateSymbol).foregroundStyle(task.stateColor).font(.headline)
                    Text(task.explanation).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }
                HStack(alignment: .top, spacing: 34) {
                    Fact(label: "Worker", value: task.assignee ?? "Not assigned")
                    Fact(label: "Reviewer", value: task.reviewer == "operator" ? "You (operator)" : task.reviewer ?? "Task creator / operator")
                }
                if task.state == "submitted" {
                    HStack {
                        Button("Accept Work") { decision = .accept }.buttonStyle(.borderedProminent)
                        Button("Request Changes…") { decision = .changes }
                    }.disabled(!store.canWrite)
                }
                Divider()
                ProseSection(title: "Task brief", text: task.brief.isEmpty ? "No brief provided." : task.brief)
                ProseSection(title: "Acceptance criteria", text: task.acceptance.isEmpty ? "No acceptance criteria provided. Review the brief before accepting." : task.acceptance)
                if !task.pathScopes.isEmpty {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("File scope").font(.headline)
                        ForEach(task.pathScopes, id: \.self) { Text($0).font(.callout.monospaced()).textSelection(.enabled) }
                    }
                }
                if let result = task.result {
                    Divider()
                    ProseSection(title: "Submitted work", text: result.summary)
                    if !result.details.isEmpty { ProseSection(title: "Details", text: result.details) }
                    if !result.changedFiles.isEmpty {
                        DisclosureGroup("Changed files (\(result.changedFiles.count))") {
                            ForEach(result.changedFiles, id: \.self) { Text($0).font(.caption.monospaced()).frame(maxWidth: .infinity, alignment: .leading).textSelection(.enabled) }
                        }
                    }
                    if !result.artifacts.isEmpty {
                        DisclosureGroup("Attached references (\(result.artifacts.count))") {
                            ForEach(Array(result.artifacts.enumerated()), id: \.offset) { _, item in
                                VStack(alignment: .leading, spacing: 2) {
                                    Text("\(item.type): \(item.value)").font(.caption.monospaced()).textSelection(.enabled)
                                    if let note = item.description, !note.isEmpty {
                                        Text(note).font(.caption).foregroundStyle(.secondary)
                                    }
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)
                            }
                        }
                    }
                    if !result.validation.isEmpty {
                        VStack(alignment: .leading, spacing: 10) {
                            Text("Worker-reported checks").font(.headline)
                            ForEach(Array(result.validation.enumerated()), id: \.offset) { _, check in
                                Label(check.summary, systemImage: check.passed == true ? "checkmark.circle" : "exclamationmark.circle")
                                    .foregroundStyle(check.passed == true ? Color.secondary : Color.orange)
                            }
                            Text("Reported by the worker, not independently verified by ACS.").font(.caption).foregroundStyle(.secondary)
                        }
                    }
                }
                if let review = task.review {
                    Divider()
                    ProseSection(title: review.accepted ? "Accepted by \(review.reviewer)" : "Changes requested by \(review.reviewer)", text: review.feedback.isEmpty ? "No written feedback." : review.feedback)
                    Text(timestamp(review.reviewedMs)).font(.caption).foregroundStyle(.secondary)
                }
                if !detail.notes.isEmpty {
                    Divider()
                    Text("Notes").font(.headline)
                    ForEach(detail.notes) { note in
                        VStack(alignment: .leading, spacing: 6) {
                            Text("\(note.author) · \(timestamp(note.tsMs))").font(.caption).foregroundStyle(.secondary)
                            Text(note.body).textSelection(.enabled)
                        }
                    }
                }
                if !detail.messages.isEmpty {
                    Divider()
                    DisclosureGroup("Task messages (\(detail.messages.count))") {
                        ForEach(detail.messages) { MessageRow(message: $0).padding(.vertical, 8) }
                    }
                }
                Divider()
                HStack {
                    Text("Updated \(timestamp(task.updatedMs))").font(.caption).foregroundStyle(.secondary)
                    Spacer()
                    if !task.isClosed {
                        Menu("Task actions") {
                            if ["claimed", "open"].contains(task.state) {
                                Button("Return to Queue…") { decision = .requeue }
                            }
                            Button("Cancel Task…", role: .destructive) { decision = .cancel }
                        }.disabled(!store.canWrite)
                    }
                }
            }.padding(28).frame(maxWidth: 760, alignment: .leading).frame(maxWidth: .infinity, alignment: .leading)
        }
        .sheet(item: $decision) { DecisionSheet(task: task, decision: $0).environmentObject(store) }
    }
}

struct Fact: View {
    let label: String
    let value: String
    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(label).font(.caption).foregroundStyle(.secondary)
            Text(value).font(.callout.weight(.medium)).textSelection(.enabled)
        }
    }
}

struct ProseSection: View {
    let title: String
    let text: String
    var body: some View {
        VStack(alignment: .leading, spacing: 9) {
            Text(title).font(.headline)
            Text(text).textSelection(.enabled).lineSpacing(4).fixedSize(horizontal: false, vertical: true)
        }
    }
}
