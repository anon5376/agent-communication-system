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
                        Text("\(filtered.count)").foregroundStyle(.secondary).monospacedDigit()
                        Spacer()
                        Button { store.showNewTask = true } label: {
                            Label("New task", systemImage: "plus")
                        }
                        .buttonStyle(.borderedProminent).controlSize(.small)
                        .disabled(!store.canWrite).help("New task (⌘N)")
                    }
                    TextField("Search tasks", text: $query).textFieldStyle(.roundedBorder)
                    if !reviewOnly { Toggle("Show completed tasks", isOn: $includeClosed).font(.caption) }
                }.padding(18)
                Divider()
                if filtered.isEmpty {
                    EmptyState(symbol: reviewOnly ? "tray" : "checklist", title: query.isEmpty ? (reviewOnly ? "All caught up" : "No tasks yet") : "No matches", message: reviewOnly ? "Submitted work will appear here, ready for a decision." : "Create a task with a clear goal and acceptance criteria.")
                } else {
                    List(selection: $store.selectedTask) {
                        if reviewOnly {
                            ForEach(filtered) { taskRow($0) }
                        } else {
                            ForEach(groupTasks(filtered), id: \.group) { section in
                                Section {
                                    ForEach(section.tasks) { taskRow($0) }
                                } header: {
                                    HStack(spacing: 5) {
                                        Text(section.group.title).font(.callout.weight(.semibold))
                                        Text("\(section.tasks.count)").monospacedDigit().foregroundStyle(.tertiary)
                                    }
                                    .foregroundStyle(groupColor(section.group))
                                    .help(section.group.hint)
                                    .textCase(nil)
                                }
                            }
                        }
                    }
                    .listStyle(.inset)
                    .listRowSeparator(.hidden)
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

    private func groupColor(_ group: TaskListGroup) -> Color {
        switch group {
        case .review: return .primary
        case .stuck: return .red
        default: return .secondary
        }
    }

    private func taskRow(_ task: TaskRecord) -> some View {
        let selected = store.selectedTask == task.id
        return HStack(alignment: .top, spacing: 10) {
            StateDot(color: selected ? .white : task.stateColor).padding(.top, 6)
            VStack(alignment: .leading, spacing: 4) {
                Text(task.title).font(.headline).lineLimit(2)
                HStack(spacing: 8) {
                    if let label = rowStateLabel(task.state) {
                        Text(label).font(.caption.weight(.semibold)).foregroundStyle(selected ? .white : task.stateColor)
                    }
                    Text(task.assignee ?? "Unassigned")
                    Text("·")
                    Text(relativeTime(task.updatedMs))
                    Spacer()
                    Text("#\(task.id)").monospacedDigit()
                }
                .font(.caption).foregroundStyle(selected ? Color.white : Color.secondary)
            }
        }
        .padding(.vertical, 7)
        .foregroundStyle(selected ? Color.white : Color.primary)
        .tag(task.id)
        .listRowBackground(
            Rectangle().fill(selected ? Color.black : Color.clear)
        )
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
                    Text(task.title).font(.system(size: 25, weight: .semibold)).textSelection(.enabled)
                    HStack(spacing: 10) {
                        Label(task.stateTitle, systemImage: task.stateSymbol)
                            .font(.caption.weight(.semibold))
                            .padding(.horizontal, 9).padding(.vertical, 4)
                            .overlay(Capsule().stroke(task.stateColor, lineWidth: 1))
                            .foregroundStyle(task.stateColor)
                        Text("Task #\(task.id)").font(.callout).foregroundStyle(.secondary).monospacedDigit()
                        if task.priority != "normal" {
                            Text("\(task.priority.capitalized) priority").font(.callout).foregroundStyle(.secondary)
                        }
                    }
                    Text(task.explanation).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }
                StageStrip(task: task)
                HStack(alignment: .top, spacing: 34) {
                    Fact(label: "Worker", value: task.assignee ?? "Not assigned")
                    Fact(label: "Reviewer", value: task.reviewer == "operator" ? "You" : task.reviewer ?? "Task creator or you")
                    Fact(label: "Last update", value: relativeTime(task.updatedMs))
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
                                    .foregroundStyle(check.passed == true ? Color.secondary : Color.red)
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
        .safeAreaInset(edge: .bottom) {
            if task.state == "submitted" {
                VStack(spacing: 0) {
                    Divider()
                    HStack(spacing: 12) {
                        Text(task.reviewer == "operator" || task.reviewer == nil
                             ? "Does this meet the acceptance criteria?"
                             : "Assigned reviewer: \(task.reviewer ?? ""). You can still decide as operator.")
                            .font(.callout).foregroundStyle(.secondary)
                        Spacer()
                        Button("Request changes…") { decision = .changes }.disabled(!store.canWrite)
                        Button("Accept work") { decision = .accept }
                            .buttonStyle(.borderedProminent).disabled(!store.canWrite)
                    }
                    .padding(.horizontal, 28).padding(.vertical, 12)
                }
                .background(.white)
            }
        }
        .sheet(item: $decision) { DecisionSheet(task: task, decision: $0).environmentObject(store) }
    }
}

/// Four-step progress strip: Created → Claimed → Submitted → Reviewed. Reached
/// steps are accent; the current step is bold with a halo and, off the happy
/// path, tinted/relabeled to the actual state.
private struct StageStrip: View {
    let task: TaskRecord

    private var progress: StageProgress { taskStage(task) }

    private func label(for index: Int) -> String {
        let progress = progress
        if index == progress.reached, progress.outcome != nil, task.state != "accepted" {
            return task.stateTitle
        }
        return taskStages[index]
    }

    private func color(for index: Int) -> Color {
        let progress = progress
        if index == progress.reached, let outcome = progress.outcome {
            switch outcome {
            case .ok, .warn: return .primary
            case .bad: return .red
            }
        }
        return index <= progress.reached ? .primary : .secondary.opacity(0.5)
    }

    var body: some View {
        let progress = progress
        HStack(alignment: .top, spacing: 0) {
            ForEach(taskStages.indices, id: \.self) { index in
                let reached = index <= progress.reached
                let current = index == progress.reached
                VStack(spacing: 7) {
                    Circle()
                        .fill(reached ? color(for: index) : Color.clear)
                        .overlay(Circle().strokeBorder(reached ? Color.clear : Color.secondary.opacity(0.6), lineWidth: 1.6))
                        .overlay(
                            Circle()
                                .stroke(color(for: index).opacity(0.28), lineWidth: 4)
                                .opacity(current ? 1 : 0)
                                .padding(-4)
                        )
                        .frame(width: 12, height: 12)
                        .padding(.top, 4)
                    Text(label(for: index))
                        .font(current ? .caption.weight(.semibold) : .caption)
                        .foregroundStyle(current && progress.outcome != nil ? color(for: index) : (reached ? Color.secondary : Color.secondary.opacity(0.6)))
                }
                if index < taskStages.count - 1 {
                    Rectangle()
                        .fill(index < progress.reached ? Color.primary : Color.secondary.opacity(0.3))
                        .frame(height: 1.5)
                        .frame(maxWidth: .infinity)
                        .padding(.top, 10)
                        .padding(.horizontal, 4)
                }
            }
        }
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
