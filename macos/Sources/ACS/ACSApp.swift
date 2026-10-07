import AppKit
import SwiftUI
import ACSCore

@main
struct ACSApp: App {
    @StateObject private var workspace = WorkspaceStore()

    var body: some Scene {
        WindowGroup("ACS") {
            WorkspaceView()
                .environmentObject(workspace)
                .frame(minWidth: 960, minHeight: 640)
                .tint(.black)
                .preferredColorScheme(.light)
                .task { await workspace.restore() }
        }
        .defaultSize(width: 1180, height: 780)
        .commands {
            CommandGroup(replacing: .newItem) {
                Button("New Task…") { workspace.showNewTask = true }
                    .keyboardShortcut("n")
                    .disabled(!workspace.canWrite)
                Divider()
                Button("Open Project…") { workspace.chooseProject() }
                    .keyboardShortcut("o")
                Button("Connect Existing Bus…") { workspace.chooseDatabase() }
                Button("Try Sample Workspace") { Task { await workspace.openSample() } }
            }
            CommandGroup(after: .toolbar) {
                Button("Communication") { workspace.mode = .communication }
                    .keyboardShortcut("1")
                Button("Orchestration") { workspace.mode = .orchestration }
                    .keyboardShortcut("2")
                Button("Refresh Workspace") { Task { await workspace.refresh() } }
                    .keyboardShortcut("r")
            }
        }
    }
}

enum Destination: String, CaseIterable, Identifiable {
    case tasks = "Tasks", reviews = "Needs review", agents = "Agents", messages = "Messages"
    case goals = "Goals", presets = "Prompt presets"
    var id: String { rawValue }
    var symbol: String {
        switch self {
        case .tasks: return "checklist"
        case .reviews: return "tray"
        case .agents: return "person.2"
        case .messages: return "bubble.left.and.bubble.right"
        case .goals: return "scope"
        case .presets: return "text.alignleft"
        }
    }
}

enum WorkspaceMode: String, CaseIterable, Identifiable {
    case communication = "Communication", orchestration = "Orchestration"
    var id: String { rawValue }
    var destinations: [Destination] {
        self == .communication ? [.messages, .tasks, .reviews] : [.goals, .presets, .agents]
    }
}

struct OrbitMark: View {
    var body: some View {
        GeometryReader { geometry in
            let size = min(geometry.size.width, geometry.size.height)
            ZStack {
                RoundedRectangle(cornerRadius: size * 0.22).fill(.black)
                Circle().stroke(.white, lineWidth: size * 0.07).padding(size * 0.2)
                ForEach(0..<3) { index in
                    Circle().fill(.white).frame(width: size * 0.17, height: size * 0.17)
                        .offset(y: -size * 0.3).rotationEffect(.degrees(Double(index) * 120))
                }
            }
        }.accessibilityHidden(true)
    }
}

extension TaskRecord {
    var stateTitle: String { taskStateTitle(state) }
    var stateSymbol: String {
        switch state {
        case "claimed": return "circle.dotted"
        case "submitted": return "tray.full"
        case "accepted": return "checkmark.circle.fill"
        case "failed", "blocked": return "exclamationmark.circle"
        case "changes_requested": return "arrow.uturn.backward.circle"
        case "cancelled": return "slash.circle"
        default: return "circle"
        }
    }
    var stateColor: Color {
        switch state {
        case "accepted": return .primary
        case "failed": return .red
        case "blocked": return .red
        case "submitted", "changes_requested", "claimed": return .primary
        default: return .secondary
        }
    }
    var explanation: String {
        switch state {
        case "open": return "Waiting for \(assignee ?? "an eligible worker") to claim this task."
        case "claimed": return "Claimed by \(assignee ?? "a worker"). Check recent messages for progress."
        case "submitted": return "Work submitted. Waiting for \(reviewer ?? "the task creator or operator") to review it."
        case "changes_requested": return "The reviewer requested changes. The task is not accepted yet."
        case "blocked": return dependencies.isEmpty ? "This task is blocked. Check its notes and messages." : "Waiting on dependencies: \(dependencies.map { "#\($0)" }.joined(separator: ", "))."
        case "failed": return "This task failed. Read its submission and messages before deciding what to do next."
        case "accepted": return "The submitted work was accepted by its reviewer."
        case "cancelled": return "This task was cancelled. Its history is preserved."
        default: return "State reported by the ACS bus: \(state)."
        }
    }
    var isClosed: Bool { ["accepted", "failed", "cancelled"].contains(state) }
}

func timestamp(_ milliseconds: Int64) -> String {
    Date(timeIntervalSince1970: Double(milliseconds) / 1000)
        .formatted(date: .abbreviated, time: .shortened)
}

func toneColor(_ tone: SubjectTone) -> Color {
    switch tone {
    case .ok, .warn: return .primary
    case .bad: return .red
    case .accent: return .primary
    case .muted: return .secondary
    }
}

/// 8pt state dot: filled in the state's tone, hollow ring when muted.
struct StateDot: View {
    let color: Color
    var body: some View {
        Group {
            if color == .secondary {
                Circle().strokeBorder(Color.secondary.opacity(0.8), lineWidth: 1.5)
            } else {
                Circle().fill(color)
            }
        }
        .frame(width: 8, height: 8)
    }
}
