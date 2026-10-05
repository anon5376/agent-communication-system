import Foundation

/// Errors surfaced by `ACSClient.request`. `helperError` carries the
/// helper's own error envelope; the other cases are transport failures.
public enum ACSError: LocalizedError, Sendable {
    /// The executable is missing, not executable, or failed to start.
    case helperUnavailable(String)
    /// The helper returned `{ok: false, error: {code, message}}`.
    case helperError(code: String, message: String)
    /// The helper exited nonzero without a parseable error envelope.
    case helperExited(status: Int32, detail: String)
    /// stdout was not the expected single JSON envelope.
    case malformedResponse(String)
    /// The helper produced more output than `outputLimit` allows.
    case outputTruncated
    /// No reply within the client's timeout; the helper was terminated.
    case timedOut(TimeInterval)

    public var errorDescription: String? {
        switch self {
        case .helperUnavailable(let detail):
            return "The ACS helper is unavailable: \(detail)"
        case .helperError(_, let message):
            return message
        case .helperExited(let status, let detail):
            return "The ACS helper exited with status \(status)."
                + (detail.isEmpty ? "" : " \(detail)")
        case .malformedResponse(let detail):
            return "The ACS helper returned an unreadable response: \(detail)"
        case .outputTruncated:
            return "The ACS helper produced more output than ACS can accept."
        case .timedOut(let seconds):
            return "The ACS helper did not answer within \(Int(seconds)) seconds."
        }
    }
}

/// Speaks to the bundled `acs-desktop` helper: one JSON request on stdin,
/// one JSON envelope on stdout. Transport only — it knows no action shapes.
///
/// Each request spawns a fresh helper process launched directly (no shell),
/// with a small set of safe tool directories prepended to the inherited PATH
/// and HOME preserved.
public actor ACSClient {
    /// Maximum bytes retained from each of the helper's stdout and stderr.
    public static let outputLimit = 16 * 1024 * 1024

    private let executable: URL
    private let database: URL
    private let timeout: TimeInterval
    private let encoder = JSONEncoder()
    private let decoder = JSONDecoder()

    public init(executable: URL, database: URL, timeout: TimeInterval = 30) {
        self.executable = executable
        self.database = database
        self.timeout = timeout
    }

    /// Send one action and decode its `data` payload as `T`.
    /// Cancelling the task terminates the helper process it spawned.
    public func request<T: Decodable & Sendable>(
        _ action: String, payload: [String: JSONValue] = [:], as type: T.Type
    ) async throws -> T {
        try Task.checkCancellation()
        let envelope: [String: JSONValue] = [
            "version": .number(1),
            "dbPath": .string(database.path),
            "action": .string(action),
            "payload": .object(payload),
        ]
        let reply = try await execute(try encoder.encode(envelope))
        return try decodeReply(reply, as: type)
    }

    private struct Reply {
        let status: Int32
        let stdout: Data
        let stderr: Data
        let stdoutTruncated: Bool
    }

    private func execute(_ input: Data) async throws -> Reply {
        guard FileManager.default.isExecutableFile(atPath: executable.path) else {
            throw ACSError.helperUnavailable("not executable: \(executable.path)")
        }

        let process = Process()
        process.executableURL = executable
        process.environment = Self.helperEnvironment()
        let stdin = Pipe()
        let stdout = Pipe()
        let stderr = Pipe()
        process.standardInput = stdin
        process.standardOutput = stdout
        process.standardError = stderr

        do {
            try process.run()
        } catch {
            throw ACSError.helperUnavailable(
                "\(executable.lastPathComponent) failed to start: \(error.localizedDescription)")
        }

        // Write the single request off the cooperative pool, then close stdin
        // so the helper sees EOF and can reply.
        DispatchQueue.global().async {
            try? stdin.fileHandleForWriting.write(contentsOf: input)
            try? stdin.fileHandleForWriting.close()
        }

        // Drain both pipes concurrently so a chatty helper can never deadlock
        // on a full pipe; retained bytes are capped at outputLimit each.
        let out = BoundedBuffer(limit: Self.outputLimit)
        let err = BoundedBuffer(limit: Self.outputLimit)
        let drained = DispatchGroup()
        drained.enter()
        DispatchQueue.global().async {
            out.drain(stdout.fileHandleForReading)
            drained.leave()
        }
        drained.enter()
        DispatchQueue.global().async {
            err.drain(stderr.fileHandleForReading)
            drained.leave()
        }

        let timedOut = Flag()
        if timeout > 0 {
            DispatchQueue.global().asyncAfter(deadline: .now() + timeout) {
                if process.isRunning {
                    timedOut.set()
                    process.terminate()
                }
            }
        }

        let status: Int32 = await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                DispatchQueue.global().async {
                    process.waitUntilExit()
                    continuation.resume(returning: process.terminationStatus)
                }
            }
        } onCancel: {
            // Only our own process is signalled — no shell, no process group.
            if process.isRunning { process.terminate() }
        }

        // Pipes reach EOF right after exit; let the drain loops finish.
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            drained.notify(queue: .global()) { continuation.resume() }
        }

        if Task.isCancelled { throw CancellationError() }
        if timedOut.isSet { throw ACSError.timedOut(timeout) }
        return Reply(
            status: status, stdout: out.data, stderr: err.data,
            stdoutTruncated: out.didOverflow)
    }

    private func decodeReply<T: Decodable>(_ reply: Reply, as type: T.Type) throws -> T {
        // An error envelope is authoritative even when the helper exits nonzero.
        guard let envelope = try? decoder.decode([String: JSONValue].self, from: reply.stdout)
        else {
            if reply.stdoutTruncated { throw ACSError.outputTruncated }
            if reply.status != 0 {
                throw ACSError.helperExited(
                    status: reply.status, detail: Self.snippet(reply.stderr))
            }
            throw ACSError.malformedResponse(
                "stdout was not a JSON object (\(reply.stdout.count) bytes)")
        }
        guard let ok = envelope["ok"]?.bool else {
            throw ACSError.malformedResponse("missing \"ok\" in reply envelope")
        }
        guard ok else {
            let failure = envelope["error"]?.objectValue
            var message = "The helper reported failure."
            var code = "helper_error"
            if let failure {
                if case .string(let value)? = failure["message"] { message = value }
                if case .string(let value)? = failure["code"] { code = value }
                else if case .number(let value)? = failure["code"] {
                    code = value.truncatingRemainder(dividingBy: 1) == 0
                        ? String(Int64(value)) : String(value)
                }
            }
            throw ACSError.helperError(code: code, message: message)
        }
        guard let data = envelope["data"], !data.isNull else {
            throw ACSError.malformedResponse("reply had no \"data\" payload")
        }
        do {
            return try decoder.decode(T.self, from: encoder.encode(data))
        } catch {
            throw ACSError.malformedResponse(
                "reply data did not match \(T.self): \(Self.snippet(Data(String(describing: error).utf8)))")
        }
    }

    /// Inherited environment plus the usual user tool directories, deduped,
    /// additions first. HOME is preserved; no login shell is consulted.
    static func helperEnvironment(
        from base: [String: String] = ProcessInfo.processInfo.environment
    ) -> [String: String] {
        var environment = base
        let home = base["HOME"] ?? NSHomeDirectory()
        environment["HOME"] = home
        var seen = Set<String>()
        let entries = [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            home + "/.local/bin",
            home + "/.cargo/bin",
            home + "/.opencode/bin",
        ] + (base["PATH"] ?? "").split(separator: ":").map(String.init)
        environment["PATH"] = entries
            .filter { !$0.isEmpty && seen.insert($0).inserted }
            .joined(separator: ":")
        return environment
    }

    /// First readable bytes of helper output, trimmed for error messages.
    private static func snippet(_ data: Data, limit: Int = 400) -> String {
        let prefix = data.prefix(limit)
        return String(decoding: prefix, as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

/// Lock-protected byte sink: retains up to `limit` bytes while still draining
/// the rest, so the child never blocks on a full pipe.
private final class BoundedBuffer: @unchecked Sendable {
    private let limit: Int
    private let lock = NSLock()
    private var storage = Data()
    private var overflow = false

    init(limit: Int) { self.limit = limit }

    var didOverflow: Bool {
        lock.lock()
        defer { lock.unlock() }
        return overflow
    }

    var data: Data {
        lock.lock()
        defer { lock.unlock() }
        return storage
    }

    func drain(_ handle: FileHandle) {
        while let chunk = try? handle.read(upToCount: 256 * 1024), !chunk.isEmpty {
            lock.lock()
            let room = limit - storage.count
            if room > 0 { storage.append(chunk.prefix(room)) }
            if chunk.count > room { overflow = true }
            lock.unlock()
        }
    }
}

/// Lock-protected one-shot flag.
private final class Flag: @unchecked Sendable {
    private let lock = NSLock()
    private var value = false

    var isSet: Bool {
        lock.lock()
        defer { lock.unlock() }
        return value
    }

    func set() {
        lock.lock()
        value = true
        lock.unlock()
    }
}
