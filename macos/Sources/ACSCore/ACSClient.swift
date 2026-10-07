import Foundation

/// Errors surfaced by `ACSClient.request`. `helperError` carries the
/// helper's own error envelope; the other cases are transport failures.
public enum ACSError: LocalizedError, Sendable {
    /// The executable is missing, not executable, or failed to start.
    case helperUnavailable(String)
    /// The helper returned `{ok: false, error: {code, message}}`.
    case helperError(code: String, message: String)
    /// The helper exited nonzero without a trusted error envelope.
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
/// and HOME preserved. Shutdown is bounded: SIGTERM, then SIGKILL on the same
/// single process if it is still alive after a short grace. Pipe reads are
/// nonblocking, so a descendant that inherited our pipes can never keep a
/// finished request waiting on EOF.
public actor ACSClient {
    /// Maximum bytes retained from each of the helper's stdout and stderr.
    public static let outputLimit = 16 * 1024 * 1024
    /// How long SIGTERM gets to work before SIGKILL lands on the same process.
    private static let killGrace: TimeInterval = 0.5

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
            "version": .integer(1),
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

    /// The helper's reply envelope, decoded straight from stdout bytes so
    /// Int64 fields keep their precision (no Double round-trip).
    private struct Response<Payload: Decodable>: Decodable {
        let ok: Bool?
        let data: Payload?
        let error: Failure?

        struct Failure: Decodable {
            let code: String
            let message: String

            private enum CodingKeys: String, CodingKey { case code, message }

            init(from decoder: Decoder) throws {
                let container = try decoder.container(keyedBy: CodingKeys.self)
                message = (try? container.decode(String.self, forKey: .message))
                    ?? "The helper reported failure."
                if let code = try? container.decode(String.self, forKey: .code) {
                    self.code = code
                } else if let code = try? container.decode(Int64.self, forKey: .code) {
                    self.code = String(code)
                } else if let code = try? container.decode(Double.self, forKey: .code) {
                    self.code = String(code)
                } else {
                    code = "helper_error"
                }
            }
        }
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

        // Observe exit through Foundation's termination callback, installed
        // before launch so a fast exit cannot be missed. Blocking a GCD thread
        // in waitUntilExit() can miss termination entirely and hang forever.
        let exited = Flag()
        let finished = Completion()
        process.terminationHandler = { finished.complete(.exited($0.terminationStatus)) }

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

        // Nonblocking reads on both pipes: a descendant that inherited them
        // must never keep this request waiting on EOF after our process exits.
        let outFD = stdout.fileHandleForReading.fileDescriptor
        let errFD = stderr.fileHandleForReading.fileDescriptor
        Self.makeNonblocking(outFD)
        Self.makeNonblocking(errFD)

        let out = BoundedBuffer(limit: Self.outputLimit)
        let err = BoundedBuffer(limit: Self.outputLimit)
        let drained = DispatchGroup()
        for (fd, buffer) in [(outFD, out), (errFD, err)] {
            drained.enter()
            DispatchQueue.global().async {
                buffer.drain(fileDescriptor: fd, done: exited)
                drained.leave()
            }
        }

        // SIGTERM first, SIGKILL on the same owned process if still alive
        // after the grace period. Never the process group: descendants are
        // the helper's own business; we just stop reading their pipes.
        let escalate = {
            if process.isRunning {
                process.terminate()
                DispatchQueue.global().asyncAfter(deadline: .now() + Self.killGrace) {
                    if process.isRunning {
                        kill(process.processIdentifier, SIGKILL)
                    }
                }
            }
        }

        // Backstop for a missed termination callback: an exited process is
        // observed within one poll interval rather than never.
        let poll = DispatchSource.makeTimerSource(queue: .global())
        poll.schedule(deadline: .now() + 0.1, repeating: 0.1)
        poll.setEventHandler {
            if !process.isRunning { finished.complete(.exited(process.terminationStatus)) }
        }
        poll.resume()
        defer { poll.cancel() }

        // Timeout and cancellation stop the helper, then stop waiting for it
        // shortly after SIGKILL even if its exit is never observed.
        let abandonLater = {
            DispatchQueue.global().asyncAfter(deadline: .now() + Self.killGrace + 1) {
                finished.complete(.abandoned)
            }
        }
        let timedOut = Flag()
        let watchdog = DispatchWorkItem {
            guard !finished.isDone else { return }
            if !process.isRunning {
                finished.complete(.exited(process.terminationStatus))
                return
            }
            timedOut.set()
            escalate()
            abandonLater()
        }
        if timeout > 0 {
            DispatchQueue.global().asyncAfter(deadline: .now() + timeout, execute: watchdog)
        }
        defer { watchdog.cancel() }

        let outcome = await withTaskCancellationHandler {
            await finished.wait()
        } onCancel: {
            escalate()
            abandonLater()
        }
        exited.set()

        // Drainers finish a bounded final sweep once `exited` is set.
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            drained.notify(queue: .global()) { continuation.resume() }
        }

        if Task.isCancelled { throw CancellationError() }
        if timedOut.isSet { throw ACSError.timedOut(timeout) }
        guard case .exited(let status) = outcome else { throw ACSError.timedOut(timeout) }
        return Reply(
            status: status, stdout: out.data, stderr: err.data,
            stdoutTruncated: out.didOverflow)
    }

    private func decodeReply<T: Decodable>(_ reply: Reply, as type: T.Type) throws -> T {
        // A truncated stdout can look like valid JSON only because its tail
        // was dropped — reject before trusting any of it.
        if reply.stdoutTruncated { throw ACSError.outputTruncated }

        // Probe the envelope without decoding `data` into T yet: a success
        // whose payload shape doesn't match T should read as malformed, not
        // as a transport failure.
        let probe = try? decoder.decode(Response<JSONValue>.self, from: reply.stdout)

        // An error envelope is authoritative even when the helper exits nonzero.
        if let failure = probe?.error, probe?.ok == false {
            throw ACSError.helperError(code: failure.code, message: failure.message)
        }
        guard probe?.ok == true else {
            if probe?.ok == false {
                throw ACSError.helperError(
                    code: "helper_error", message: "The helper reported failure.")
            }
            if reply.status != 0 {
                throw ACSError.helperExited(
                    status: reply.status, detail: Self.snippet(reply.stderr))
            }
            throw ACSError.malformedResponse(
                "stdout was not a JSON envelope (\(reply.stdout.count) bytes)")
        }
        // Success requires a clean exit; an ok:true reply from a crashed
        // helper cannot be trusted.
        guard reply.status == 0 else {
            throw ACSError.helperExited(
                status: reply.status, detail: Self.snippet(reply.stderr))
        }
        do {
            guard let payload = try decoder.decode(Response<T>.self, from: reply.stdout).data
            else {
                throw ACSError.malformedResponse("reply had no \"data\" payload")
            }
            return payload
        } catch let error as ACSError {
            throw error
        } catch {
            throw ACSError.malformedResponse(
                "reply data did not match \(T.self): "
                    + Self.snippet(Data(String(describing: error).utf8)))
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

    private static func makeNonblocking(_ fd: Int32) {
        let flags = fcntl(fd, F_GETFL)
        guard flags >= 0 else { return }
        _ = fcntl(fd, F_SETFL, flags | O_NONBLOCK)
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

    /// Nonblocking read loop: appends while the owning process runs, and once
    /// `done` is set, performs a bounded final sweep of whatever bytes remain
    /// (a descendant holding the pipe open only forfeits further output).
    func drain(fileDescriptor fd: Int32, done: Flag) {
        var chunk = [UInt8](repeating: 0, count: 256 * 1024)
        var postDoneBudget = limit
        while true {
            if done.isSet && postDoneBudget <= 0 { return }
            let count = chunk.withUnsafeMutableBytes {
                read(fd, $0.baseAddress, $0.count)
            }
            if count > 0 {
                if done.isSet { postDoneBudget -= count }
                lock.lock()
                let room = limit - storage.count
                if room > 0 { storage.append(contentsOf: chunk[..<min(count, room)]) }
                if count > room { overflow = true }
                lock.unlock()
                continue
            }
            if count == 0 { return } // EOF
            if errno == EINTR { continue }
            if errno == EAGAIN {
                if done.isSet { return }
                usleep(1000)
                continue
            }
            return // real read error
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

/// One-shot helper outcome: the first completion wins and resumes the waiter.
private final class Completion: @unchecked Sendable {
    enum Outcome: Sendable {
        case exited(Int32)
        case abandoned
    }

    private let lock = NSLock()
    private var outcome: Outcome?
    private var waiter: CheckedContinuation<Outcome, Never>?

    var isDone: Bool {
        lock.lock()
        defer { lock.unlock() }
        return outcome != nil
    }

    func complete(_ value: Outcome) {
        lock.lock()
        guard outcome == nil else {
            lock.unlock()
            return
        }
        outcome = value
        let pending = waiter
        waiter = nil
        lock.unlock()
        pending?.resume(returning: value)
    }

    func wait() async -> Outcome {
        await withCheckedContinuation { continuation in
            lock.lock()
            if let outcome {
                lock.unlock()
                continuation.resume(returning: outcome)
                return
            }
            waiter = continuation
            lock.unlock()
        }
    }
}
