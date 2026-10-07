import Foundation
import XCTest
@testable import ACSCore

final class OrchestrationTests: XCTestCase {
    func testDecodesEmptyCrewAndLargeGoalIdentifiers() throws {
        let json = #"{"configured":false,"crewError":null,"crewDir":"/workspace/aos","goalOwner":null,"missions":[],"roles":[],"crew":[],"goals":[{"id":9007199254740993,"title":"Queued goal","state":"open","assignee":null,"reviewer":"operator","updatedMs":123}]}"#
        let value = try JSONDecoder().decode(Orchestration.self, from: Data(json.utf8))
        XCTAssertFalse(value.configured)
        XCTAssertNil(value.goalOwner)
        XCTAssertEqual(value.goals.first?.id, 9_007_199_254_740_993)
        XCTAssertEqual(value.goals.first?.state, "open")
    }

    func testOnlyCanonicalRolePathsCanOpenPresetEditor() throws {
        for (path, expected) in [("roles/builder.md", "builder" as String?), ("/custom/builder.md", nil), ("roles/../secret.md", nil), ("builder.md", nil)] {
            let json = try JSONSerialization.data(withJSONObject: [
                "id": "builder", "role": "implementation", "cli": "local", "description": "Builds", "enabled": false,
                "instructions": path, "running": false,
            ])
            let member = try JSONDecoder().decode(CrewMember.self, from: json)
            XCTAssertEqual(member.editableRole, expected, path)
            XCTAssertFalse(member.enabled)
        }
    }
}
