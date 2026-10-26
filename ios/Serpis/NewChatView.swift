import SwiftUI

/// A new chat: on which machine, and in which folder there.
struct NewChatView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    let started: (ChatModel) -> Void
    @State private var machine: Machine?

    var body: some View {
        NavigationStack {
            Group {
                if let machine {
                    FolderList(machine: machine, path: "~", started: started)
                } else {
                    List(app.machines) { m in
                        Button { machine = m } label: {
                            MachineRow(machine: m, link: app.link(m.id))
                        }
                        .buttonStyle(.plain)
                    }
                    .navigationTitle("New chat")
                }
            }
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
            }
            .onAppear {
                if app.machines.count == 1 { machine = app.machines[0] }
            }
        }
    }
}

/// The folders in `path` on a machine: recent ones first at its home, a tap opens one, and a
/// chat starts in the one shown.
struct FolderList: View {
    @Environment(AppModel.self) private var app
    let machine: Machine
    let path: String
    let started: (ChatModel) -> Void
    @State private var dirs: [String] = []
    @State private var shown = ""
    @State private var problem: String?
    @State private var starting = false

    var body: some View {
        List {
            if path == "~" {
                let recent = app.recentFolders(on: machine.id).prefix(5)
                if !recent.isEmpty {
                    Section("Recent") {
                        ForEach(Array(recent), id: \.self) { dir in
                            Button { start(in: dir) } label: {
                                Label((dir as NSString).abbreviatingWithTildeInPath, systemImage: "clock")
                            }
                        }
                    }
                }
            }
            Section(path == "~" ? "Folders" : "") {
                ForEach(dirs, id: \.self) { d in
                    NavigationLink((d as NSString).lastPathComponent) {
                        FolderList(machine: machine, path: (path as NSString).appendingPathComponent(d), started: started)
                    }
                }
            }
        }
        .overlay {
            if let problem { ContentUnavailableView("Can't list folders", systemImage: "exclamationmark.triangle", description: Text(problem)) }
        }
        .navigationTitle(path == "~" ? machine.name : (path as NSString).lastPathComponent)
        .safeAreaInset(edge: .bottom) {
            Button { start(in: path) } label: {
                Text(starting ? "Starting…" : "Start a chat here")
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 6)
            }
            .buttonStyle(.borderedProminent)
            .disabled(starting || problem != nil)
            .padding()
            .background(.bar)
        }
        .task {
            await app.connect(machine)
            do {
                let found = try await app.folders(on: machine.id, in: path)
                (shown, dirs) = (found.path, found.dirs.filter { !$0.hasPrefix(".") })
            } catch {
                problem = AppModel.describe(error)
            }
        }
    }

    private func start(in dir: String) {
        starting = true
        Task {
            do {
                started(try await app.newChat(on: machine.id, in: dir))
            } catch {
                problem = AppModel.describe(error)
            }
            starting = false
        }
    }
}
