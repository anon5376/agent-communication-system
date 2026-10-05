import Foundation

// MARK: - Task grouping for the list

/// Sections of the task list, in display order. Named `TaskListGroup` to avoid
/// colliding with Swift concurrency's `TaskGroup`.
public enum TaskListGroup: String, CaseIterable, Sendable {
    case review, stuck, active, queued, done

    public var title: String {
        switch self {
        case .review: return "Needs review"
        case .stuck: return "Stuck"
        case .active: return "In progress"
        case .queued: return "Queued"
        case .done: return "Finished"
        }
    }

    public var hint: String {
        switch self {
        case .review: return "Submitted work waiting for a decision"
        case .stuck: return "Blocked or failed"
        case .active: return "An agent is working on it"
        case .queued: return "Waiting for a worker"
        case .done: return "Accepted or cancelled"
        }
    }
}

public struct TaskGroupSection: Sendable {
    public let group: TaskListGroup
    public let tasks: [TaskRecord]

    public init(group: TaskListGroup, tasks: [TaskRecord]) {
        self.group = group
        self.tasks = tasks
    }
}

public func taskGroup(of state: String) -> TaskListGroup {
    switch state {
    case "submitted": return .review
    case "blocked", "failed": return .stuck
    case "claimed", "changes_requested": return .active
    case "accepted", "cancelled": return .done
    default: return .queued
    }
}

/// Non-empty sections in display order; preserves the input ordering within
/// each section.
public func groupTasks(_ tasks: [TaskRecord]) -> [TaskGroupSection] {
    TaskListGroup.allCases.compactMap { group in
        let inGroup = tasks.filter { taskGroup(of: $0.state) == group }
        return inGroup.isEmpty ? nil : TaskGroupSection(group: group, tasks: inGroup)
    }
}

/// The label a task state wears when the group header doesn't already say it.
public func taskStateTitle(_ state: String) -> String {
    switch state {
    case "open": return "Queued"
    case "claimed": return "Claimed"
    case "submitted": return "Needs review"
    case "changes_requested": return "Changes requested"
    case "blocked": return "Blocked"
    case "accepted": return "Accepted"
    case "cancelled": return "Cancelled"
    case "failed": return "Failed"
    default: return state.replacingOccurrences(of: "_", with: " ").capitalized
    }
}

/// The state label a row needs only when its group header doesn't already say it.
public func rowStateLabel(_ state: String) -> String? {
    switch state {
    case "changes_requested", "failed", "cancelled": return taskStateTitle(state)
    default: return nil
    }
}

// MARK: - Progress strip

public let taskStages = ["Created", "Claimed", "Submitted", "Reviewed"]

public enum StageOutcome: String, Sendable, Equatable {
    case ok, warn, bad
}

/// Index of the last reached stage, and whether the task ended off the happy
/// path (`warn`/`bad`) or completed it (`ok`). Off the happy path the current
/// step is relabeled to the state title by the caller.
public struct StageProgress: Sendable, Equatable {
    public let reached: Int
    public let outcome: StageOutcome?

    public init(reached: Int, outcome: StageOutcome?) {
        self.reached = reached
        self.outcome = outcome
    }
}

public func taskStage(_ task: TaskRecord) -> StageProgress {
    switch task.state {
    case "open": return StageProgress(reached: 0, outcome: nil)
    case "blocked": return StageProgress(reached: task.assignee != nil ? 1 : 0, outcome: .warn)
    case "claimed": return StageProgress(reached: 1, outcome: nil)
    case "changes_requested": return StageProgress(reached: 1, outcome: .warn)
    case "submitted": return StageProgress(reached: 2, outcome: nil)
    case "accepted": return StageProgress(reached: 3, outcome: .ok)
    case "failed": return StageProgress(reached: task.result != nil ? 2 : 1, outcome: .bad)
    case "cancelled":
        return StageProgress(reached: task.result != nil ? 2 : (task.assignee != nil ? 1 : 0), outcome: .bad)
    default: return StageProgress(reached: 0, outcome: nil)
    }
}

// MARK: - Relative time

public func relativeTime(_ milliseconds: Int64, now: Int64? = nil) -> String {
    let reference = now ?? Int64(Date().timeIntervalSince1970 * 1000)
    let diff = max(0, reference - milliseconds)
    let minutes = diff / 60_000
    if minutes < 1 { return "just now" }
    if minutes < 60 { return "\(minutes) min ago" }
    let hours = minutes / 60
    if hours < 24 { return "\(hours) h ago" }
    let days = hours / 24
    if days < 7 { return days == 1 ? "yesterday" : "\(days) days ago" }
    return Date(timeIntervalSince1970: Double(milliseconds) / 1000)
        .formatted(.dateTime.month(.abbreviated).day())
}

// MARK: - Message subject tags

public enum SubjectTone: String, Sendable, Equatable {
    case ok, warn, bad, accent, muted
}

/// A parsed "[KIND #id rN] rest" bus subject tag.
public struct SubjectTag: Sendable, Equatable {
    public let label: String
    public let tone: SubjectTone
    public let taskID: Int64
    public let round: Int?
    public let rest: String

    public init(label: String, tone: SubjectTone, taskID: Int64, round: Int?, rest: String) {
        self.label = label
        self.tone = tone
        self.taskID = taskID
        self.round = round
        self.rest = rest
    }
}

private let subjectTagLabels: [String: (label: String, tone: SubjectTone)] = [
    "DONE": ("Submitted", .warn),
    "CHANGES": ("Changes requested", .warn),
    "ACCEPTED": ("Accepted", .ok),
    "FAILED": ("Failed", .bad),
    "BLOCKED": ("Blocked", .warn),
    "TASK": ("New task", .accent),
]

// swiftlint:disable:next force_try
private let subjectTagPattern = try! NSRegularExpression(
    pattern: #"^\[([A-Z]+) #(\d+)(?: r(\d+))?\]\s*(.*)$"#,
    options: [.dotMatchesLineSeparators]
)

/// "[DONE #7 r1] title" -> { label: "Submitted", taskID: 7, round: 1, rest: "title" }
public func parseSubjectTag(_ subject: String) -> SubjectTag? {
    let range = NSRange(subject.startIndex..<subject.endIndex, in: subject)
    guard let match = subjectTagPattern.firstMatch(in: subject, range: range),
          let kindRange = Range(match.range(at: 1), in: subject),
          let idRange = Range(match.range(at: 2), in: subject),
          let restRange = Range(match.range(at: 4), in: subject),
          let taskID = Int64(subject[idRange])
    else { return nil }
    let kind = String(subject[kindRange])
    let round = Range(match.range(at: 3), in: subject).flatMap { Int(subject[$0]) }
    let known = subjectTagLabels[kind]
    let label = known?.label ?? kind.prefix(1) + kind.dropFirst().lowercased()
    return SubjectTag(
        label: label,
        tone: known?.tone ?? .muted,
        taskID: taskID,
        round: round,
        rest: String(subject[restRange])
    )
}
