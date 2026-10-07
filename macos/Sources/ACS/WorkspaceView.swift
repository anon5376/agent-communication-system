import SwiftUI
import ACSCore

struct WorkspaceView: View {
    @EnvironmentObject private var store: WorkspaceStore

    var body: some View {
        NavigationSplitView {
            VStack(alignment: .leading, spacing: 0) {
                HStack(spacing: 10) {
                    OrbitMark().frame(width: 36, height: 36)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("ACS").font(.title2.bold())
                        Text("Agent workspace").font(.caption).foregroundStyle(.secondary)
                    }
                }.padding(20)
                Picker("Workspace mode", selection: $store.mode) {
                    ForEach(WorkspaceMode.allCases) { Text($0.rawValue).tag($0) }
                }.pickerStyle(.segmented).labelsHidden().padding(.horizontal, 12).padding(.bottom, 16)
                VStack(spacing: 4) {
                    ForEach(store.mode.destinations) { destination in
                        Button { store.destination = destination } label: {
                            HStack {
                                Label(destination.rawValue, systemImage: destination.symbol)
                                Spacer()
                                if destination == .reviews, store.reviewCount > 0 {
                                    Text("\(store.reviewCount)").font(.caption.monospacedDigit())
                                }
                            }
                            .padding(.horizontal, 10).padding(.vertical, 9)
                            .foregroundStyle(store.destination == destination ? Color.white : Color.black)
                            .background(store.destination == destination ? Color.black : Color.clear)
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityAddTraits(store.destination == destination ? .isSelected : [])
                    }
                }.padding(.horizontal, 12)
                Spacer(minLength: 0)
                VStack(alignment: .leading, spacing: 10) {
                    Divider()
                    if store.snapshot != nil, store.destination != .agents {
                        Button {
                            store.mode = .orchestration
                            store.destination = .agents
                        } label: {
                            Label("Configure agents", systemImage: "slider.horizontal.3")
                                .font(.callout.weight(.semibold)).padding(.vertical, 6)
                        }.buttonStyle(.plain)
                    }
                    Label(store.name, systemImage: store.snapshot?.simulated == true ? "sparkles" : "folder")
                        .font(.callout.weight(.medium)).lineLimit(2)
                    Menu {
                        Button("Open Project…", action: store.chooseProject)
                        Button("Connect Existing Bus…", action: store.chooseDatabase)
                        Button("Try Sample Workspace") { Task { await store.openSample() } }
                        Divider()
                        Button("Show Database in Finder", action: store.revealDatabase)
                            .disabled(store.database == nil)
                    } label: { Text("Switch workspace").font(.caption) }
                    .disabled(store.busy)
                    Label("Local on this Mac", systemImage: "internaldrive")
                        .font(.caption2).foregroundStyle(.secondary)
                }.padding(16)
            }
            .background(Color.white)
            .navigationSplitViewColumnWidth(min: 260, ideal: 280, max: 320)
        } detail: {
            VStack(spacing: 0) {
                if store.snapshot?.simulated == true {
                    Banner(symbol: "sparkles", text: "Sample workspace · simulated agents, no model calls or charges", color: .primary)
                }
                if let error = store.error {
                    Banner(symbol: "exclamationmark.triangle", text: error, color: .red) { store.error = nil }
                } else if let notice = store.notice {
                    Banner(symbol: "checkmark.circle", text: notice, color: .primary) { store.notice = nil }
                }
                if store.snapshot == nil {
                    WelcomeView()
                } else {
                    if store.snapshot?.canOperate == false {
                        Banner(symbol: "lock", text: "Read-only connection. The operator identity is unavailable for this bus.", color: .primary)
                    }
                    if store.mode == .orchestration, let error = store.orchestrationError ?? store.orchestration?.crewError {
                        Banner(symbol: "exclamationmark.triangle", text: error, color: .red)
                    }
                    switch store.destination ?? .tasks {
                    case .tasks, .reviews: TaskBrowser(reviewOnly: store.destination == .reviews)
                    case .agents: AgentsView()
                    case .messages: MessagesView()
                    case .goals: GoalsView()
                    case .presets: PresetsView()
                    }
                }
                Divider()
                HStack(spacing: 8) {
                    if store.busy || store.refreshing { ProgressView().controlSize(.mini) }
                    Text(store.busy ? "Working…" : "Your tasks and history stay on your Mac.")
                    Spacer()
                    if let date = store.lastRefresh {
                        Text("Updated \(date.formatted(date: .omitted, time: .standard))").monospacedDigit()
                    }
                }.font(.caption).foregroundStyle(.secondary).padding(.horizontal, 18).padding(.vertical, 8)
            }
            .navigationTitle(store.snapshot == nil ? "Welcome to ACS" : store.name)
            .toolbar {
                ToolbarItemGroup(placement: .primaryAction) {
                    Button { Task { await store.refresh() } } label: { Label("Refresh", systemImage: "arrow.clockwise") }
                        .help("Refresh workspace (⌘R)").disabled(store.database == nil || store.busy)
                    Button { store.showNewTask = true } label: { Label("New Task", systemImage: "plus") }
                        .disabled(!store.canWrite)
                }
            }
        }
        .sheet(isPresented: $store.showNewTask) { NewTaskSheet().environmentObject(store) }
        .task(id: "\(store.database?.path ?? "")/\(store.mode.rawValue)") {
            if store.mode == .orchestration { await store.loadOrchestration() }
        }
        .task {
            while !Task.isCancelled {
                do { try await Task.sleep(for: .seconds(4)) } catch { break }
                await store.refresh()
            }
        }
    }
}

struct WelcomeView: View {
    @EnvironmentObject private var store: WorkspaceStore
    var body: some View {
        VStack(alignment: .leading, spacing: 26) {
            OrbitMark().frame(width: 64, height: 64)
            VStack(alignment: .leading, spacing: 12) {
                Text("Different agents.\nOne place to work.")
                    .font(.system(size: 36, weight: .semibold, design: .rounded))
                Text("Give your coding agents a task, see who owns it, and review what comes back. No terminal needed to manage the work.")
                    .font(.title3).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
            VStack(alignment: .leading, spacing: 18) {
                WelcomeStep(number: "1", title: "Choose a project", subtitle: "Keep its tasks, messages, and review history together.")
                WelcomeStep(number: "2", title: "Connect your agents", subtitle: "Use coding CLIs you already have. You choose when they run.")
                WelcomeStep(number: "3", title: "Give work a clear finish line", subtitle: "Write the task and acceptance criteria, then review the result.")
            }
            HStack(spacing: 12) {
                Button("Open a Project…", action: store.chooseProject).buttonStyle(.borderedProminent)
                Button("Try the Sample") { Task { await store.openSample() } }.buttonStyle(.bordered)
            }.controlSize(.large).disabled(store.busy)
            Text("The sample is free and simulated. No account, API key, or model call.")
                .font(.caption).foregroundStyle(.secondary)
            Button("Already use ACS? Connect an existing bus…", action: store.chooseDatabase)
                .buttonStyle(.plain).foregroundStyle(.primary).underline().disabled(store.busy)
        }
        .frame(maxWidth: 560, alignment: .leading).padding(48)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

private struct WelcomeStep: View {
    let number: String
    let title: String
    let subtitle: String
    var body: some View {
        HStack(alignment: .top, spacing: 14) {
            Text(number).font(.callout.weight(.semibold)).foregroundStyle(.primary)
                .frame(width: 26, height: 26).overlay(Circle().stroke(.secondary, lineWidth: 1))
            VStack(alignment: .leading, spacing: 3) {
                Text(title).font(.headline)
                Text(subtitle).foregroundStyle(.secondary)
            }
        }
    }
}

struct Banner: View {
    let symbol: String
    let text: String
    let color: Color
    var dismiss: (() -> Void)? = nil
    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: symbol).foregroundStyle(color)
            Text(text).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
            if let dismiss {
                Button(action: dismiss) { Image(systemName: "xmark") }
                    .buttonStyle(.plain).accessibilityLabel("Dismiss message")
            }
        }.font(.callout).padding(.horizontal, 18).padding(.vertical, 10)
            .background(.white)
            .overlay(alignment: .bottom) { Divider() }
    }
}

struct EmptyState: View {
    let symbol: String
    let title: String
    let message: String
    var body: some View {
        VStack(spacing: 14) {
            Image(systemName: symbol).font(.system(size: 38, weight: .light)).foregroundStyle(.secondary)
            Text(title).font(.title2.weight(.semibold))
            Text(message).foregroundStyle(.secondary).multilineTextAlignment(.center).frame(maxWidth: 340)
        }.padding(30).frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
