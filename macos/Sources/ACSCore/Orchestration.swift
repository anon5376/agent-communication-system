import Foundation

public struct Orchestration: Decodable, Sendable {
    public let configured: Bool
    public let crewError: String?
    public let crewDir: String
    public let goalOwner: String?
    public let missions: [MissionPrompt]
    public let roles: [RolePrompt]
    public let crew: [CrewMember]
    public let goals: [GoalRecord]
}

public struct MissionPrompt: Decodable, Sendable, Identifiable {
    public let name: String
    public let summary: String
    public let brief: String
    public let acceptance: String
    public let text: String
    public let custom: Bool
    public var id: String { name }
}

public struct RolePrompt: Decodable, Sendable, Identifiable {
    public let name: String
    public let text: String
    public let custom: Bool
    public var id: String { name }
}

public struct CrewMember: Decodable, Sendable, Identifiable {
    public let id: String
    public let role: String
    public let cli: String
    public let description: String
    public let enabled: Bool
    public let instructions: String?
    public let running: Bool

    public var editableRole: String? {
        guard let instructions, instructions.hasPrefix("roles/"), instructions.hasSuffix(".md") else { return nil }
        let name = String(instructions.dropFirst(6).dropLast(3))
        return !name.isEmpty && name.allSatisfy { $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-" || $0 == "_") } ? name : nil
    }
}

public struct GoalRecord: Decodable, Sendable, Identifiable {
    public let id: Int64
    public let title: String
    public let state: String
    public let assignee: String?
    public let reviewer: String?
    public let updatedMs: Int64
}
