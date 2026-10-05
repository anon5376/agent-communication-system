import XCTest
@testable import ACSCore

final class PresentationTests: XCTestCase {

    private func makeTask(
        id: Int64 = 1,
        state: String,
        assignee: String? = nil,
        result: TaskResult? = nil
    ) -> TaskRecord {
        TaskRecord(
            id: id, title: "t\(id)", brief: "", acceptance: "", state: state,
            priority: "normal", assignee: assignee,
            updatedMs: 0, createdMs: 0, result: result
        )
    }

    private let someResult = TaskResult(
        summary: "done", details: "", changedFiles: [], validation: []
    )

    // MARK: Grouping

    func testTaskGroupMapping() {
        XCTAssertEqual(taskGroup(of: "submitted"), .review)
        XCTAssertEqual(taskGroup(of: "blocked"), .stuck)
        XCTAssertEqual(taskGroup(of: "failed"), .stuck)
        XCTAssertEqual(taskGroup(of: "claimed"), .active)
        XCTAssertEqual(taskGroup(of: "changes_requested"), .active)
        XCTAssertEqual(taskGroup(of: "open"), .queued)
        XCTAssertEqual(taskGroup(of: "accepted"), .done)
        XCTAssertEqual(taskGroup(of: "cancelled"), .done)
        XCTAssertEqual(taskGroup(of: "something-else"), .queued)
    }

    func testGroupTasksOrdersAndHidesEmpty() {
        let groups = groupTasks([
            makeTask(id: 1, state: "open"),
            makeTask(id: 2, state: "submitted"),
            makeTask(id: 3, state: "open"),
        ])
        XCTAssertEqual(groups.map(\.group), [.review, .queued])
        XCTAssertEqual(groups[0].tasks.map(\.id), [2])
        XCTAssertEqual(groups[1].tasks.map(\.id), [1, 3])
    }

    func testRowStateLabel() {
        XCTAssertEqual(rowStateLabel("changes_requested"), "Changes requested")
        XCTAssertEqual(rowStateLabel("failed"), "Failed")
        XCTAssertEqual(rowStateLabel("cancelled"), "Cancelled")
        XCTAssertNil(rowStateLabel("submitted"))
        XCTAssertNil(rowStateLabel("open"))
        XCTAssertNil(rowStateLabel("accepted"))
    }

    // MARK: Stage strip

    func testTaskStageHappyPath() {
        XCTAssertEqual(taskStage(makeTask(state: "open")), StageProgress(reached: 0, outcome: nil))
        XCTAssertEqual(taskStage(makeTask(state: "claimed")), StageProgress(reached: 1, outcome: nil))
        XCTAssertEqual(taskStage(makeTask(state: "submitted")), StageProgress(reached: 2, outcome: nil))
        XCTAssertEqual(taskStage(makeTask(state: "accepted")), StageProgress(reached: 3, outcome: .ok))
    }

    func testTaskStageOffPath() {
        XCTAssertEqual(
            taskStage(makeTask(state: "blocked")),
            StageProgress(reached: 0, outcome: .warn)
        )
        XCTAssertEqual(
            taskStage(makeTask(state: "blocked", assignee: "impl-a")),
            StageProgress(reached: 1, outcome: .warn)
        )
        XCTAssertEqual(
            taskStage(makeTask(state: "changes_requested")),
            StageProgress(reached: 1, outcome: .warn)
        )
        XCTAssertEqual(
            taskStage(makeTask(state: "failed")),
            StageProgress(reached: 1, outcome: .bad)
        )
        XCTAssertEqual(
            taskStage(makeTask(state: "failed", result: someResult)),
            StageProgress(reached: 2, outcome: .bad)
        )
        XCTAssertEqual(
            taskStage(makeTask(state: "cancelled")),
            StageProgress(reached: 0, outcome: .bad)
        )
        XCTAssertEqual(
            taskStage(makeTask(state: "cancelled", assignee: "impl-a")),
            StageProgress(reached: 1, outcome: .bad)
        )
        XCTAssertEqual(
            taskStage(makeTask(state: "cancelled", assignee: "impl-a", result: someResult)),
            StageProgress(reached: 2, outcome: .bad)
        )
    }

    // MARK: Relative time

    func testRelativeTime() {
        let now: Int64 = 1_000_000_000
        XCTAssertEqual(relativeTime(now - 30_000, now: now), "just now")
        XCTAssertEqual(relativeTime(now - 5 * 60_000, now: now), "5 min ago")
        XCTAssertEqual(relativeTime(now - 59 * 60_000, now: now), "59 min ago")
        XCTAssertEqual(relativeTime(now - 3 * 3_600_000, now: now), "3 h ago")
        XCTAssertEqual(relativeTime(now - 26 * 3_600_000, now: now), "yesterday")
        XCTAssertEqual(relativeTime(now - 3 * 24 * 3_600_000, now: now), "3 days ago")
        XCTAssertEqual(relativeTime(now - 6 * 24 * 3_600_000, now: now), "6 days ago")
        // Older than a week falls back to a short date (timezone-dependent, so
        // just check it stops being a relative phrase).
        let old = relativeTime(0, now: 8 * 24 * 3_600_000)
        XCTAssertFalse(old.contains("ago"))
        XCTAssertNotEqual(old, "yesterday")
        XCTAssertNotEqual(old, "just now")
        // Future timestamps clamp to zero.
        XCTAssertEqual(relativeTime(now + 60_000, now: now), "just now")
    }

    // MARK: Subject tags

    func testParseSubjectTagKnownKinds() {
        let submitted = parseSubjectTag("[DONE #7 r1] hard usd cap")
        XCTAssertEqual(
            submitted,
            SubjectTag(label: "Submitted", tone: .warn, taskID: 7, round: 1, rest: "hard usd cap")
        )
        let accepted = parseSubjectTag("[ACCEPTED #12] x")
        XCTAssertEqual(
            accepted,
            SubjectTag(label: "Accepted", tone: .ok, taskID: 12, round: nil, rest: "x")
        )
        XCTAssertEqual(parseSubjectTag("[CHANGES #5 r2] y")?.label, "Changes requested")
        XCTAssertEqual(parseSubjectTag("[CHANGES #5 r2] y")?.round, 2)
        XCTAssertEqual(parseSubjectTag("[FAILED #3] z")?.tone, .bad)
        XCTAssertEqual(parseSubjectTag("[BLOCKED #3] z")?.tone, .warn)
        XCTAssertEqual(parseSubjectTag("[TASK #9] z")?.label, "New task")
        XCTAssertEqual(parseSubjectTag("[TASK #9] z")?.tone, .accent)
    }

    func testParseSubjectTagUnknownKind() {
        let tag = parseSubjectTag("[QUESTION #4] what now")
        XCTAssertEqual(tag?.label, "Question")
        XCTAssertEqual(tag?.tone, .muted)
        XCTAssertEqual(tag?.taskID, 4)
    }

    func testParseSubjectTagNonMatches() {
        XCTAssertNil(parseSubjectTag("plain subject"))
        XCTAssertNil(parseSubjectTag("[done #7] lowercase kind"))
        XCTAssertNil(parseSubjectTag("[DONE] no id"))
        XCTAssertNil(parseSubjectTag("prefix [DONE #7] rest"))
    }
}
