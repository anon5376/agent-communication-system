import XCTest
import ACSCore

/// Transport tests using fake `acs-desktop` helpers: tiny executable shell
/// scripts written to a temp dir. No real providers are ever invoked.
final class ClientTransportTests: XCTestCase {
    private var tempDir: URL!

    override func setUpWithError() throws {
        tempDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("acscore-tests-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: tempDir)
    }

    /// Write an executable fake helper. `body` is /bin/sh.
    private func makeHelper(_ body: String, name: String = "acs-desktop") throws -> URL {
        let url = tempDir.appendingPathComponent(name)
        try "#!/bin/sh\n\(body)\n".write(to: url, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes(
            [.posixPermissions: 0o755], ofItemAtPath: url.path)
        return url
    }

    private func client(
        _ helper: URL, timeout: TimeInterval = 10
    ) -> ACSClient {
        ACSClient(
            executable: helper,
            database: tempDir.appendingPathComponent("bus.db"),
            timeout: timeout)
    }

    func testSuccessEnvelopeDecodesAndRequestIsWellFormed() async throws {
        let requestFile = tempDir.appendingPathComponent("request.json")
        let helper = try makeHelper("""
            cat > "\(requestFile.path)"
            echo '{"ok":true,"data":{"message":"all done"}}'
            """)
        let reply: Acknowledgement = try await client(helper)
            .request("send", payload: [
                "to": .string("rusty"),
                "subject": .string("hi"),
                "n": .number(3),
            ], as: Acknowledgement.self)
        XCTAssertEqual(reply.message, "all done")

        // The helper saw exactly one protocol request on stdin.
        let sent = try JSONDecoder().decode(
            [String: JSONValue].self, from: Data(contentsOf: requestFile))
        XCTAssertEqual(sent["version"], .integer(1))
        XCTAssertEqual(sent["dbPath"], .string(tempDir.appendingPathComponent("bus.db").path))
        XCTAssertEqual(sent["action"], .string("send"))
        XCTAssertEqual(
            sent["payload"],
            .object(["to": .string("rusty"), "subject": .string("hi"), "n": .integer(3)]))
    }

    func testIntegerPayloadPreservesIdsBeyondDoublePrecision() async throws {
        let requestFile = tempDir.appendingPathComponent("request.json")
        let helper = try makeHelper("""
            cat > "\(requestFile.path)"
            echo '{"ok":true,"data":{"message":"ok"}}'
            """)
        let big: Int64 = 9_007_199_254_740_993 // 2^53 + 1
        let _: Acknowledgement = try await client(helper)
            .request("task", payload: ["id": .integer(big)], as: Acknowledgement.self)
        let raw = try String(contentsOf: requestFile, encoding: .utf8)
        XCTAssertTrue(
            raw.contains("9007199254740993"),
            "integer payload must not degrade to a Double: \(raw)")
        let sent = try JSONDecoder().decode(
            [String: JSONValue].self, from: Data(contentsOf: requestFile))
        XCTAssertEqual(sent["payload"], .object(["id": .integer(big)]))
    }

    func testResponsePreservesLargeInt64Ids() async throws {
        let helper = try makeHelper("""
            cat > /dev/null
            echo '{"ok":true,"data":{"id":9007199254740993,"title":"big","brief":"","acceptance":"","state":"open","priority":"normal","assignee":null,"reviewer":null,"project":null,"pathScopes":[],"dependencies":[],"updatedMs":1,"createdMs":1,"result":null,"review":null,"notes":[],"messages":[]}}'
            """)
        let detail: TaskDetail = try await client(helper)
            .request("task", payload: ["id": .integer(9)], as: TaskDetail.self)
        // 2^53 + 1 rounds to 9007199254740992 through a Double.
        XCTAssertEqual(detail.task.id, 9_007_199_254_740_993)
    }

    func testSuccessReplyOnNonzeroExitIsRejected() async throws {
        let helper = try makeHelper("""
            cat > /dev/null
            echo '{"ok":true,"data":{"message":"hmm"}}'
            exit 1
            """)
        do {
            let _: Acknowledgement = try await client(helper)
                .request("snapshot", as: Acknowledgement.self)
            XCTFail("expected helperExited")
        } catch let ACSError.helperExited(status, _) {
            XCTAssertEqual(status, 1)
        }
    }

    func testTimeoutEscalatesWhenHelperIgnoresSigterm() async throws {
        let helper = try makeHelper("""
            trap '' TERM
            while true; do sleep 1; done
            """)
        let started = Date()
        do {
            let _: Acknowledgement = try await client(helper, timeout: 0.5)
                .request("snapshot", as: Acknowledgement.self)
            XCTFail("expected timedOut")
        } catch ACSError.timedOut {
            // expected
        }
        XCTAssertLessThan(
            Date().timeIntervalSince(started), 15,
            "SIGTERM-ignoring helper must be SIGKILLed within the grace period")
    }

    func testDescendantHoldingPipesDoesNotBlockShutdown() async throws {
        // The helper spawns a child that inherits our stdout/stderr pipes and
        // outlives it, then sleeps forever itself. The timeout path must not
        // wait for the pipe EOF the descendant will never produce.
        let pidFile = tempDir.appendingPathComponent("child.pid")
        let helper = try makeHelper("""
            sleep 60 &
            echo $! > "\(pidFile.path)"
            exec sleep 60
            """)
        defer {
            if let text = try? String(contentsOf: pidFile, encoding: .utf8),
               let pid = Int32(text.trimmingCharacters(in: .whitespacesAndNewlines)) {
                kill(pid, SIGKILL)
            }
        }
        let started = Date()
        do {
            let _: Acknowledgement = try await client(helper, timeout: 0.5)
                .request("snapshot", as: Acknowledgement.self)
            XCTFail("expected timedOut")
        } catch ACSError.timedOut {
            // expected
        }
        XCTAssertLessThan(
            Date().timeIntervalSince(started), 15,
            "a descendant holding our pipes open must not block the reply")
    }

    func testErrorEnvelopeWinsOverNonZeroExit() async throws {
        let helper = try makeHelper("""
            cat > /dev/null
            echo '{"ok":false,"error":{"code":"unknown_action","message":"nope"}}'
            exit 2
            """)
        do {
            let _: Acknowledgement = try await client(helper)
                .request("bogus", as: Acknowledgement.self)
            XCTFail("expected helperError")
        } catch let ACSError.helperError(code, message) {
            XCTAssertEqual(code, "unknown_action")
            XCTAssertEqual(message, "nope")
        }
    }

    func testNonZeroExitWithoutEnvelopeReportsStatusAndStderr() async throws {
        let helper = try makeHelper("""
            cat > /dev/null
            echo 'helper exploded' >&2
            exit 3
            """)
        do {
            let _: Acknowledgement = try await client(helper)
                .request("snapshot", as: Acknowledgement.self)
            XCTFail("expected helperExited")
        } catch let ACSError.helperExited(status, detail) {
            XCTAssertEqual(status, 3)
            XCTAssertTrue(detail.contains("helper exploded"), "detail was: \(detail)")
        }
    }

    func testMalformedStdoutIsRejected() async throws {
        let helper = try makeHelper("""
            cat > /dev/null
            echo 'this is not json'
            """)
        do {
            let _: Acknowledgement = try await client(helper)
                .request("snapshot", as: Acknowledgement.self)
            XCTFail("expected malformedResponse")
        } catch ACSError.malformedResponse {
            // expected
        }
    }

    func testMissingDataPayloadIsRejected() async throws {
        let helper = try makeHelper("""
            cat > /dev/null
            echo '{"ok":true}'
            """)
        do {
            let _: Acknowledgement = try await client(helper)
                .request("snapshot", as: Acknowledgement.self)
            XCTFail("expected malformedResponse")
        } catch ACSError.malformedResponse {
            // expected
        }
    }

    func testTimeoutTerminatesHelper() async throws {
        let helper = try makeHelper("sleep 30")
        let started = Date()
        do {
            let _: Acknowledgement = try await client(helper, timeout: 0.5)
                .request("snapshot", as: Acknowledgement.self)
            XCTFail("expected timedOut")
        } catch ACSError.timedOut {
            // expected
        }
        XCTAssertLessThan(
            Date().timeIntervalSince(started), 15,
            "the sleeping helper should have been killed well before its exit")
    }

    func testCancellationTerminatesHelper() async throws {
        let helper = try makeHelper("sleep 30")
        let task = Task {
            try await client(helper, timeout: 60)
                .request("snapshot", as: Acknowledgement.self)
        }
        try await Task.sleep(for: .milliseconds(300))
        task.cancel()
        do {
            _ = try await task.value
            XCTFail("expected CancellationError")
        } catch is CancellationError {
            // expected
        }
    }

    func testOversizedStdoutIsBounded() async throws {
        // ~20 MiB of junk, far over the 16 MiB output limit.
        let helper = try makeHelper("""
            cat > /dev/null
            yes x | head -c 20000000
            """)
        do {
            let _: Acknowledgement = try await client(helper)
                .request("snapshot", as: Acknowledgement.self)
            XCTFail("expected outputTruncated")
        } catch ACSError.outputTruncated {
            // expected
        }
    }

    func testMissingHelperFailsFast() async throws {
        let absent = tempDir.appendingPathComponent("no-such-helper")
        do {
            let _: Acknowledgement = try await client(absent)
                .request("snapshot", as: Acknowledgement.self)
            XCTFail("expected helperUnavailable")
        } catch ACSError.helperUnavailable {
            // expected
        }
    }

    func testHelperSeesAugmentedPathAndHome() async throws {
        let envFile = tempDir.appendingPathComponent("env.txt")
        let helper = try makeHelper("""
            cat > /dev/null
            printf 'PATH=%s\\nHOME=%s\\n' "$PATH" "$HOME" > "\(envFile.path)"
            echo '{"ok":true,"data":{"message":"ok"}}'
            """)
        let _: Acknowledgement = try await client(helper)
            .request("snapshot", as: Acknowledgement.self)
        let env = try String(contentsOf: envFile, encoding: .utf8)
        for dir in ["/opt/homebrew/bin", "/usr/local/bin", ".local/bin", ".cargo/bin", ".opencode/bin"] {
            XCTAssertTrue(env.contains(dir), "PATH missing \(dir): \(env)")
        }
        XCTAssertTrue(env.contains("HOME=/"), "HOME not preserved: \(env)")
    }
}
