import AppKit
import CryptoKit
import SwiftUI
import ACSCore

@MainActor
final class WorkspaceStore: ObservableObject {
    @Published var snapshot: Snapshot?
    @Published var detail: TaskDetail?
    @Published var providers: [ProviderRecord] = []
    @Published var destination: Destination? = .tasks
    @Published var selectedTask: Int64?
    @Published var database: URL?
    @Published var project: URL?
    @Published var error: String?
    @Published var notice: String?
    @Published var busy = false
    @Published var refreshing = false
    @Published var detailLoading = false
    @Published var detailError: String?
    @Published var showNewTask = false
    @Published var lastRefresh: Date?
    private var client: ACSClient?
    private var restored = false
    private var generation = UUID()

    var canWrite: Bool { snapshot?.canOperate == true && !busy }
    var name: String { snapshot?.simulated == true ? "Sample workspace" : project?.lastPathComponent ?? database?.deletingLastPathComponent().lastPathComponent ?? "Your workspace" }
    var agents: [AgentRecord] { snapshot?.agents.filter { $0.id != "operator" } ?? [] }
    var tasks: [TaskRecord] { snapshot?.tasks ?? [] }
    var reviewCount: Int { tasks.filter { $0.state == "submitted" }.count }
    var support: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("ACS", isDirectory: true)
    }

    private func helper() throws -> URL {
        let url = Bundle.main.bundleURL.appendingPathComponent("Contents/Helpers/acs-desktop")
        guard FileManager.default.isExecutableFile(atPath: url.path) else {
            throw NSError(domain: "ACS", code: 1, userInfo: [NSLocalizedDescriptionKey: "ACS’s local helper is missing. Install the complete ACS.app from the DMG, not the standalone Swift executable."])
        }
        return url
    }

    func restore() async {
        guard !restored else { return }
        restored = true
        guard let path = UserDefaults.standard.string(forKey: "acs.database") else { return }
        let folder = UserDefaults.standard.string(forKey: "acs.project").map { URL(fileURLWithPath: $0) }
        await connect(URL(fileURLWithPath: path), project: folder)
    }

    func chooseProject() {
        guard !busy else { return }
        let panel = NSOpenPanel()
        panel.title = "Choose a project for ACS"
        panel.message = "Your tasks stay local. No agents will start until you explicitly start them."
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.prompt = "Open Project"
        guard panel.runModal() == .OK, let folder = panel.url else { return }
        let key = SHA256.hash(data: Data(folder.resolvingSymlinksInPath().path.utf8)).map { String(format: "%02x", $0) }.joined()
        let db = support.appendingPathComponent("Workspaces/\(key)/bus.db")
        Task { await connect(db, project: folder, initialize: !FileManager.default.fileExists(atPath: db.path)) }
    }

    func chooseDatabase() {
        guard !busy else { return }
        let panel = NSOpenPanel()
        panel.title = "Connect an existing ACS bus"
        panel.message = "Choose a trusted ACS bus.db. ACS may apply its normal additive database migrations."
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.prompt = "Connect"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        Task { await connect(url, project: nil) }
    }

    func openSample() async {
        guard !busy else { return }
        let db = support.appendingPathComponent("Samples/\(UUID().uuidString)/bus.db")
        await connect(db, project: nil, demo: true)
    }

    private func connect(_ db: URL, project folder: URL?, initialize: Bool = false, demo: Bool = false) async {
        guard !busy else { return }
        busy = true
        defer { busy = false }
        error = nil
        do {
            let next = ACSClient(executable: try helper(), database: db)
            if initialize || demo {
                try FileManager.default.createDirectory(at: db.deletingLastPathComponent(), withIntermediateDirectories: true)
                let _: Acknowledgement = try await next.request(demo ? "demo" : "init", as: Acknowledgement.self)
            }
            let state: Snapshot = try await next.request("snapshot", as: Snapshot.self)
            generation = UUID()
            client = next
            database = db
            project = folder
            snapshot = state
            providers = []
            detail = nil
            selectedTask = nil
            notice = nil
            destination = .tasks
            lastRefresh = Date()
            UserDefaults.standard.set(db.path, forKey: "acs.database")
            UserDefaults.standard.set(folder?.path, forKey: "acs.project")
        } catch { self.error = error.localizedDescription }
    }

    func refresh() async {
        guard let client, !refreshing, !busy else { return }
        let current = generation
        refreshing = true
        defer { refreshing = false }
        do {
            let state: Snapshot = try await client.request("snapshot", as: Snapshot.self)
            guard current == generation else { return }
            snapshot = state
            lastRefresh = Date()
            error = nil
            await loadDetail()
        } catch { if current == generation { self.error = error.localizedDescription } }
    }

    func loadDetail() async {
        guard let id = selectedTask, let client else { detail = nil; return }
        let current = generation
        detailLoading = true
        detailError = nil
        do {
            let next: TaskDetail = try await client.request("task", payload: ["id": .number(Double(id))], as: TaskDetail.self)
            guard current == generation, selectedTask == id else { return }
            detail = next
        } catch {
            guard current == generation, selectedTask == id else { return }
            detail = nil
            detailError = error.localizedDescription
        }
        if current == generation, selectedTask == id { detailLoading = false }
    }

    @discardableResult
    func mutate(_ action: String, _ payload: [String: JSONValue] = [:]) async -> Bool {
        guard let client, canWrite else { return false }
        busy = true
        error = nil
        notice = nil
        do {
            let reply: Acknowledgement = try await client.request(action, payload: payload, as: Acknowledgement.self)
            notice = reply.message
            busy = false
            await refresh()
            return true
        } catch {
            busy = false
            self.error = error.localizedDescription
            // A multi-agent action can partially succeed; always reload authoritative state.
            let actionError = self.error
            await refresh()
            self.error = actionError
            return false
        }
    }

    func detect() async {
        guard let client, !busy else { return }
        let current = generation
        busy = true
        defer { busy = false }
        do {
            let found: [ProviderRecord] = try await client.request("detect", as: [ProviderRecord].self)
            if current == generation { providers = found }
        } catch { self.error = error.localizedDescription }
    }

    func revealDatabase() {
        if let database { NSWorkspace.shared.activateFileViewerSelecting([database]) }
    }
}
