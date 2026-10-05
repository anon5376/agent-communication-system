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
    let message: MessageRecord
    var body: some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack {
                Text("\(message.sender) → \(message.recipient ?? "Everyone")").font(.caption.weight(.medium))
                Spacer()
                Text(timestamp(message.tsMs)).font(.caption).foregroundStyle(.secondary)
            }
            Text(message.subject).font(.headline).textSelection(.enabled)
            Text(message.body).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
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
