import Foundation

/// Simple acknowledgement returned by mutating helper actions.
public struct Acknowledgement: Decodable, Sendable {
    public let message: String

    public init(message: String) {
        self.message = message
    }
}

/// Point-in-time workspace state reported by the helper's `snapshot` action.
public struct Snapshot: Decodable, Sendable {
    public let dbPath: String
    public let simulated: Bool
    public let canOperate: Bool
    public let agents: [AgentRecord]
    public let tasks: [TaskRecord]
    public let messages: [MessageRecord]
    public let truncated: Bool

    public init(
        dbPath: String, simulated: Bool, canOperate: Bool,
        agents: [AgentRecord], tasks: [TaskRecord], messages: [MessageRecord],
        truncated: Bool
    ) {
        self.dbPath = dbPath
        self.simulated = simulated
        self.canOperate = canOperate
        self.agents = agents
        self.tasks = tasks
        self.messages = messages
        self.truncated = truncated
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        dbPath = try container.decodeIfPresent(String.self, forKey: .dbPath) ?? ""
        simulated = try container.decodeIfPresent(Bool.self, forKey: .simulated) ?? false
        canOperate = try container.decodeIfPresent(Bool.self, forKey: .canOperate) ?? false
        agents = try container.decodeIfPresent([AgentRecord].self, forKey: .agents) ?? []
        tasks = try container.decodeIfPresent([TaskRecord].self, forKey: .tasks) ?? []
        messages = try container.decodeIfPresent([MessageRecord].self, forKey: .messages) ?? []
        truncated = try container.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
    }

    private enum CodingKeys: String, CodingKey {
        case dbPath, simulated, canOperate, agents, tasks, messages, truncated
    }
}

/// One crew agent plus supervisor liveness, as joined by the helper.
public struct AgentRecord: Decodable, Sendable, Identifiable {
    public let id: String
    public let role: String
    public let model: String
    public let harness: String
    public let status: String
    public let lastSeenMs: Int64?
    public let running: Bool
    public let paused: Bool

    public init(
        id: String, role: String, model: String, harness: String, status: String,
        lastSeenMs: Int64? = nil, running: Bool = false, paused: Bool = false
    ) {
        self.id = id
        self.role = role
        self.model = model
        self.harness = harness
        self.status = status
        self.lastSeenMs = lastSeenMs
        self.running = running
        self.paused = paused
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        role = try container.decodeIfPresent(String.self, forKey: .role) ?? ""
        model = try container.decodeIfPresent(String.self, forKey: .model) ?? ""
        harness = try container.decodeIfPresent(String.self, forKey: .harness) ?? ""
        status = try container.decodeIfPresent(String.self, forKey: .status) ?? "unknown"
        lastSeenMs = try container.decodeIfPresent(Int64.self, forKey: .lastSeenMs)
        running = try container.decodeIfPresent(Bool.self, forKey: .running) ?? false
        paused = try container.decodeIfPresent(Bool.self, forKey: .paused) ?? false
    }

    private enum CodingKeys: String, CodingKey {
        case id, role, model, harness, status, lastSeenMs, running, paused
    }
}

/// The task fields the workspace UI needs; the helper may send more, which are ignored.
public struct TaskRecord: Decodable, Sendable, Identifiable {
    public let id: Int64
    public let title: String
    public let brief: String
    public let acceptance: String
    public let state: String
    public let priority: String
    public let assignee: String?
    public let reviewer: String?
    public let project: String?
    public let pathScopes: [String]
    public let dependencies: [Int64]
    public let updatedMs: Int64
    public let createdMs: Int64
    public let result: TaskResult?
    public let review: TaskReview?

    public init(
        id: Int64, title: String, brief: String, acceptance: String, state: String,
        priority: String, assignee: String? = nil, reviewer: String? = nil,
        project: String? = nil, pathScopes: [String] = [], dependencies: [Int64] = [],
        updatedMs: Int64, createdMs: Int64, result: TaskResult? = nil,
        review: TaskReview? = nil
    ) {
        self.id = id
        self.title = title
        self.brief = brief
        self.acceptance = acceptance
        self.state = state
        self.priority = priority
        self.assignee = assignee
        self.reviewer = reviewer
        self.project = project
        self.pathScopes = pathScopes
        self.dependencies = dependencies
        self.updatedMs = updatedMs
        self.createdMs = createdMs
        self.result = result
        self.review = review
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(Int64.self, forKey: .id)
        title = try container.decode(String.self, forKey: .title)
        brief = try container.decodeIfPresent(String.self, forKey: .brief) ?? ""
        acceptance = try container.decodeIfPresent(String.self, forKey: .acceptance) ?? ""
        state = try container.decode(String.self, forKey: .state)
        priority = try container.decodeIfPresent(String.self, forKey: .priority) ?? "normal"
        assignee = try container.decodeIfPresent(String.self, forKey: .assignee)
        reviewer = try container.decodeIfPresent(String.self, forKey: .reviewer)
        project = try container.decodeIfPresent(String.self, forKey: .project)
        pathScopes = try container.decodeIfPresent([String].self, forKey: .pathScopes) ?? []
        dependencies = try container.decodeIfPresent([Int64].self, forKey: .dependencies) ?? []
        updatedMs = try container.decode(Int64.self, forKey: .updatedMs)
        createdMs = try container.decodeIfPresent(Int64.self, forKey: .createdMs) ?? updatedMs
        result = try container.decodeIfPresent(TaskResult.self, forKey: .result)
        review = try container.decodeIfPresent(TaskReview.self, forKey: .review)
    }

    private enum CodingKeys: String, CodingKey {
        case id, title, brief, acceptance, state, priority, assignee, reviewer
        case project, pathScopes, dependencies, updatedMs, createdMs, result, review
    }
}

/// What a worker reported when submitting a task.
public struct TaskResult: Decodable, Sendable {
    public let summary: String
    public let details: String
    public let changedFiles: [String]
    public let validation: [ValidationObservation]

    public init(
        summary: String, details: String, changedFiles: [String],
        validation: [ValidationObservation]
    ) {
        self.summary = summary
        self.details = details
        self.changedFiles = changedFiles
        self.validation = validation
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        summary = try container.decodeIfPresent(String.self, forKey: .summary) ?? ""
        details = try container.decodeIfPresent(String.self, forKey: .details) ?? ""
        changedFiles = try container.decodeIfPresent([String].self, forKey: .changedFiles) ?? []
        validation = try container.decodeIfPresent([ValidationObservation].self, forKey: .validation) ?? []
    }

    private enum CodingKeys: String, CodingKey {
        case summary, details, changedFiles, validation
    }
}

/// One worker-reported validation check. `passed` is nil when the helper
/// could not determine the outcome.
public struct ValidationObservation: Decodable, Sendable {
    public let passed: Bool?
    public let summary: String

    public init(passed: Bool? = nil, summary: String) {
        self.passed = passed
        self.summary = summary
    }
}

/// The latest review decision on a task.
public struct TaskReview: Decodable, Sendable {
    public let reviewer: String
    public let accepted: Bool
    public let feedback: String
    public let reviewedMs: Int64

    public init(reviewer: String, accepted: Bool, feedback: String, reviewedMs: Int64) {
        self.reviewer = reviewer
        self.accepted = accepted
        self.feedback = feedback
        self.reviewedMs = reviewedMs
    }
}

/// One bus message. The helper also sends a uuid `id`; identity here is the
/// durable sequence number, which is stable and sortable.
public struct MessageRecord: Decodable, Sendable, Identifiable {
    public let seq: Int64
    public let tsMs: Int64
    public let sender: String
    public let recipient: String?
    public let subject: String
    public let body: String

    public var id: Int64 { seq }

    public init(
        seq: Int64, tsMs: Int64, sender: String, recipient: String? = nil,
        subject: String, body: String
    ) {
        self.seq = seq
        self.tsMs = tsMs
        self.sender = sender
        self.recipient = recipient
        self.subject = subject
        self.body = body
    }

    private enum CodingKeys: String, CodingKey {
        case seq, tsMs, sender, recipient, subject, body
    }
}

/// A note attached to a task's history.
public struct TaskNote: Decodable, Sendable, Identifiable {
    public let id: Int64
    public let author: String
    public let tsMs: Int64
    public let body: String

    public init(id: Int64, author: String, tsMs: Int64, body: String) {
        self.id = id
        self.author = author
        self.tsMs = tsMs
        self.body = body
    }

    private enum CodingKeys: String, CodingKey {
        case id, author, tsMs, body
    }
}

/// Helper `task` action reply: the Rust TaskDetail shape flattens the task's
/// own fields into the top-level object alongside `notes` and `messages`.
public struct TaskDetail: Decodable, Sendable {
    public let task: TaskRecord
    public let notes: [TaskNote]
    public let messages: [MessageRecord]

    public init(task: TaskRecord, notes: [TaskNote], messages: [MessageRecord]) {
        self.task = task
        self.notes = notes
        self.messages = messages
    }

    public init(from decoder: Decoder) throws {
        task = try TaskRecord(from: decoder)
        let container = try decoder.container(keyedBy: CodingKeys.self)
        notes = try container.decodeIfPresent([TaskNote].self, forKey: .notes) ?? []
        messages = try container.decodeIfPresent([MessageRecord].self, forKey: .messages) ?? []
    }

    private enum CodingKeys: String, CodingKey {
        case notes, messages
    }
}

/// One supported provider CLI found (or not found) by `detect`.
public struct ProviderRecord: Decodable, Sendable, Identifiable {
    public let id: String
    public let name: String
    public let path: String?
    public let signIn: String
    public let requiresApproval: Bool

    public init(
        id: String, name: String, path: String? = nil, signIn: String,
        requiresApproval: Bool
    ) {
        self.id = id
        self.name = name
        self.path = path
        self.signIn = signIn
        self.requiresApproval = requiresApproval
    }
}
