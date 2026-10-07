import SwiftUI
import ACSCore

struct MessagesView: View {
    @EnvironmentObject private var store: WorkspaceStore
    @State private var composing = false
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Messages").font(.largeTitle.weight(.semibold))
                    Text("The latest 100 messages across this workspace. Reading here does not acknowledge an agent’s inbox.")
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button("New Message…") { composing = true }.disabled(!store.canWrite)
            }.padding(28)
            Divider()
            if store.snapshot?.messages.isEmpty != false {
                EmptyState(symbol: "bubble.left.and.bubble.right", title: "No messages yet", message: "Agent handoffs, questions, and feedback will appear here.")
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach((store.snapshot?.messages ?? []).sorted { $0.seq > $1.seq }) { message in
                            MessageRow(message: message).padding(.vertical, 20)
                            Divider()
                        }
                    }.padding(.horizontal, 28)
                }
            }
        }.sheet(isPresented: $composing) { ComposeMessageSheet().environmentObject(store) }
    }
}

struct MessageRow: View {
    @EnvironmentObject private var store: WorkspaceStore
    let message: MessageRecord
    @State private var expanded = false

    private var tag: SubjectTag? { parseSubjectTag(message.subject) }
    private var isLong: Bool {
        message.body.count > 360 || message.body.components(separatedBy: .newlines).count > 5
    }
    private var tagTask: TaskRecord? {
        tag.flatMap { tag in store.tasks.first { $0.id == tag.taskID } }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack {
                (Text(message.sender).fontWeight(.semibold)
                    + Text(" → ").foregroundColor(.secondary)
                    + Text(message.recipient ?? "Everyone"))
                    .font(.caption).foregroundStyle(.secondary)
                Spacer()
                Text(relativeTime(message.tsMs)).font(.caption).foregroundStyle(.secondary)
                    .help(timestamp(message.tsMs))
            }
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                if let tag {
                    Text(tag.round.map { $0 > 1 ? "\(tag.label), round \($0)" : tag.label } ?? tag.label)
                        .font(.caption.weight(.semibold))
                        .padding(.horizontal, 8).padding(.vertical, 2)
                        .overlay(Capsule().stroke(toneColor(tag.tone), lineWidth: 1))
                        .foregroundStyle(toneColor(tag.tone))
                }
                Text(tag?.rest ?? message.subject).font(.headline).textSelection(.enabled)
                Spacer()
                if let tag {
                    if tagTask != nil {
                        Button("Open task #\(tag.taskID)") {
                            store.destination = .tasks
                            store.selectedTask = tag.taskID
                        }
                        .buttonStyle(.plain).font(.callout).foregroundStyle(.primary).underline()
                    } else {
                        Text("#\(tag.taskID)").font(.caption).foregroundStyle(.tertiary).monospacedDigit()
                    }
                }
            }
            Text(message.body).textSelection(.enabled)
                .lineLimit(isLong && !expanded ? 4 : nil)
                .fixedSize(horizontal: false, vertical: true)
            if isLong {
                Button(expanded ? "Show less" : "Show more") { expanded.toggle() }
                    .buttonStyle(.plain).font(.callout).foregroundStyle(.primary).underline()
            }
        }.frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct ComposeMessageSheet: View {
    @EnvironmentObject private var store: WorkspaceStore
    @Environment(\.dismiss) private var dismiss
    @State private var to = "*"
    @State private var subject = ""
    @State private var message = ""
    @State private var failure: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("New message").font(.title2.weight(.semibold))
            Picker("To", selection: $to) {
                Text("Everyone (broadcast)").tag("*")
                ForEach(store.agents) { Text($0.id).tag($0.id) }
            }
            TextField("Subject", text: $subject)
            TextEditor(text: $message).frame(height: 170).border(.quaternary).accessibilityLabel("Message body")
            Text("Messages may wake an already running agent. They do not start stopped agents.").font(.caption).foregroundStyle(.secondary)
            if let failure { Text(failure).foregroundStyle(.red) }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction).disabled(store.busy)
                Button("Send Message") {
                    Task {
                        if await store.mutate("send", ["to": .string(to), "subject": .string(subject), "body": .string(message)]) { dismiss() }
                        else { failure = store.error ?? "The message could not be sent." }
                    }
                }.buttonStyle(.borderedProminent)
                    .disabled(!store.canWrite || subject.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }.padding(24).frame(width: 540).interactiveDismissDisabled(store.busy)
    }
}
