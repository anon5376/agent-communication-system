import XCTest
import ACSCore

/// Pure decoding tests for the DTO surface the SwiftUI layer consumes.
final class DecodingTests: XCTestCase {
    private let decoder = JSONDecoder()

    func testSnapshotDecodesFullyPopulated() throws {
        let json = """
        {
          "dbPath": "/tmp/ws/bus.db",
          "simulated": false,
          "canOperate": true,
          "agents": [
            {"id": "rusty", "role": "worker", "model": "qwen3:8b", "harness": "ollama",
             "status": "idle", "lastSeenMs": 1720000000000, "running": true, "paused": false}
          ],
          "tasks": [
            {"id": 7, "title": "Wire the bus", "brief": "Do it", "acceptance": "It works",
             "state": "submitted", "priority": "high", "assignee": "rusty",
             "reviewer": "operator", "project": "demo", "pathScopes": ["src/"],
             "dependencies": [3, 4], "updatedMs": 1720000001000, "createdMs": 1720000000000,
             "result": {"summary": "Done", "details": "All of it",
                        "changedFiles": ["a.swift"], "artifacts": [],
                        "validation": [{"passed": true, "summary": "tests pass"}],
                        "completedMs": 1720000000900},
             "review": {"reviewer": "operator", "accepted": true, "feedback": "lgtm",
                        "reviewedMs": 1720000002000},
             "role": "worker", "creator": "operator", "round": 1, "attempts": 1,
             "maxRetries": 2, "refs": []}
          ],
          "messages": [
            {"seq": 42, "id": "3f2c1c6e-uuid-ignored", "tsMs": 1720000000500,
             "sender": "rusty", "recipient": "operator", "type": "result",
             "subject": "hello", "body": "world", "thread": "task:7",
             "taskId": 7, "refs": [], "requiresAck": false, "source": "bus"}
          ],
          "truncated": false
        }
        """
        let snapshot = try decoder.decode(Snapshot.self, from: Data(json.utf8))
        XCTAssertEqual(snapshot.dbPath, "/tmp/ws/bus.db")
        XCTAssertTrue(snapshot.canOperate)
        XCTAssertEqual(snapshot.agents.count, 1)
        XCTAssertEqual(snapshot.agents[0].id, "rusty")
        XCTAssertEqual(snapshot.agents[0].lastSeenMs, 1720000000000)
        XCTAssertTrue(snapshot.agents[0].running)
        XCTAssertFalse(snapshot.agents[0].paused)

        XCTAssertEqual(snapshot.tasks.count, 1)
        let task = snapshot.tasks[0]
        XCTAssertEqual(task.id, 7)
        XCTAssertEqual(task.priority, "high")
        XCTAssertEqual(task.assignee, "rusty")
        XCTAssertEqual(task.pathScopes, ["src/"])
        XCTAssertEqual(task.dependencies, [3, 4])
        XCTAssertEqual(task.result?.summary, "Done")
        XCTAssertEqual(task.result?.changedFiles, ["a.swift"])
        XCTAssertEqual(task.result?.validation.first?.passed, true)
        XCTAssertEqual(task.review?.reviewer, "operator")
        XCTAssertEqual(task.review?.accepted, true)

        XCTAssertEqual(snapshot.messages.count, 1)
        XCTAssertEqual(snapshot.messages[0].id, 42) // identity is seq, not the uuid
        XCTAssertEqual(snapshot.messages[0].recipient, "operator")
        XCTAssertFalse(snapshot.truncated)
    }

    func testSnapshotDecodesNullOptionals() throws {
        let json = """
        {
          "dbPath": "/tmp/bus.db",
          "simulated": true,
          "canOperate": false,
          "agents": [
            {"id": "ghost", "role": "worker", "model": "", "harness": "none",
             "status": "offline", "lastSeenMs": null, "running": false, "paused": false}
          ],
          "tasks": [
            {"id": 1, "title": "Bare", "brief": "", "acceptance": "", "state": "open",
             "priority": "normal", "assignee": null, "reviewer": null, "project": null,
             "pathScopes": [], "dependencies": [],
             "updatedMs": 5, "createdMs": 4, "result": null, "review": null}
          ],
          "messages": [
            {"seq": 1, "tsMs": 5, "sender": "operator", "recipient": null,
             "subject": "hi", "body": ""}
          ],
          "truncated": true
        }
        """
        let snapshot = try decoder.decode(Snapshot.self, from: Data(json.utf8))
        XCTAssertTrue(snapshot.simulated)
        XCTAssertNil(snapshot.agents[0].lastSeenMs)
        let task = snapshot.tasks[0]
        XCTAssertNil(task.assignee)
        XCTAssertNil(task.reviewer)
        XCTAssertNil(task.project)
        XCTAssertNil(task.result)
        XCTAssertNil(task.review)
        XCTAssertEqual(task.createdMs, 4)
        XCTAssertNil(snapshot.messages[0].recipient)
        XCTAssertTrue(snapshot.truncated)
    }

    func testSnapshotRejectsMissingSecurityFields() throws {
        // dbPath/simulated/canOperate/truncated are required: an empty object
        // must not decode as a usable snapshot.
        XCTAssertThrowsError(try decoder.decode(
            Snapshot.self, from: Data("{\"agents\":[],\"tasks\":[],\"messages\":[]}".utf8)))
        // ...while the collections themselves stay tolerant of omission.
        let sparse = try decoder.decode(Snapshot.self, from: Data("""
            {"dbPath": "/tmp/bus.db", "simulated": false, "canOperate": true,
             "truncated": false}
            """.utf8))
        XCTAssertTrue(sparse.tasks.isEmpty)
        XCTAssertTrue(sparse.canOperate)
    }

    func testTaskDetailDecodesFlattenedShape() throws {
        // Rust serializes TaskDetail with #[serde(flatten)]: the task's own
        // fields live at the top level next to notes/messages/dependents/leases.
        let json = """
        {
          "id": 9, "title": "Flattened", "brief": "b", "acceptance": "a",
          "state": "claimed", "priority": "normal", "assignee": "rusty",
          "reviewer": "operator", "project": null, "pathScopes": [],
          "dependencies": [], "updatedMs": 10, "createdMs": 9,
          "result": null, "review": null,
          "role": "worker", "creator": "operator",
          "notes": [
            {"id": 3, "taskId": 9, "author": "rusty", "tsMs": 11, "body": "started"}
          ],
          "dependents": [12],
          "messages": [
            {"seq": 8, "id": "uuid-ignored", "tsMs": 12, "sender": "rusty",
             "recipient": null, "subject": "progress", "body": "halfway"}
          ],
          "leases": ["rusty"]
        }
        """
        let detail = try decoder.decode(TaskDetail.self, from: Data(json.utf8))
        XCTAssertEqual(detail.task.id, 9)
        XCTAssertEqual(detail.task.title, "Flattened")
        XCTAssertEqual(detail.task.state, "claimed")
        XCTAssertNil(detail.task.project)
        XCTAssertEqual(detail.notes.count, 1)
        XCTAssertEqual(detail.notes[0].id, 3)
        XCTAssertEqual(detail.notes[0].author, "rusty")
        XCTAssertEqual(detail.messages.count, 1)
        XCTAssertEqual(detail.messages[0].seq, 8)
        XCTAssertEqual(detail.messages[0].id, 8)
    }

    func testTaskDetailDecodesEmptyCollections() throws {
        let json = """
        {
          "id": 2, "title": "Solo", "brief": "", "acceptance": "", "state": "open",
          "priority": "normal", "assignee": null, "reviewer": null, "project": null,
          "pathScopes": [], "dependencies": [], "updatedMs": 1, "createdMs": 1,
          "result": null, "review": null, "notes": [], "dependents": [],
          "messages": [], "leases": []
        }
        """
        let detail = try decoder.decode(TaskDetail.self, from: Data(json.utf8))
        XCTAssertEqual(detail.task.id, 2)
        XCTAssertTrue(detail.notes.isEmpty)
        XCTAssertTrue(detail.messages.isEmpty)
    }

    func testProviderRecordDecodes() throws {
        let json = """
        [
          {"id": "claude", "name": "Claude Code", "path": "/usr/local/bin/claude",
           "signIn": "claude login", "requiresApproval": false},
          {"id": "ollama", "name": "Ollama", "path": null,
           "signIn": "", "requiresApproval": true}
        ]
        """
        let providers = try decoder.decode([ProviderRecord].self, from: Data(json.utf8))
        XCTAssertEqual(providers.count, 2)
        XCTAssertEqual(providers[0].path, "/usr/local/bin/claude")
        XCTAssertNil(providers[1].path)
        XCTAssertTrue(providers[1].requiresApproval)
    }

    func testJSONValueRoundTrips() throws {
        let value: JSONValue = .object([
            "a": .string("x"), "b": .number(2.5), "c": .bool(true),
            "d": .array([.null, .number(1)]), "e": .null,
        ])
        let encoded = try JSONEncoder().encode(value)
        let decoded = try decoder.decode(JSONValue.self, from: encoded)
        guard case .object(let object) = decoded else {
            return XCTFail("expected object, got \(decoded)")
        }
        XCTAssertEqual(object["a"], .string("x"))
        XCTAssertEqual(object["b"], .number(2.5))
        XCTAssertEqual(object["c"], .bool(true))
        XCTAssertEqual(object["e"], .null)
    }
}
