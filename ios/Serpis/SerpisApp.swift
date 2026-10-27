import SwiftUI

@main
struct SerpisApp: App {
    @State private var app = AppModel(store: SerpisApp.store())
    @Environment(\.scenePhase) private var phase

    var body: some Scene {
        WindowGroup {
            Group {
                if let broken = app.broken {
                    ContentUnavailableView("Serpis can't start", systemImage: "exclamationmark.triangle", description: Text(broken))
                } else {
                    ChatsView()
                }
            }
            .environment(app)
            // A desktop's pairing code (the Camera app opens it here).
            .onOpenURL { url in
                if url.scheme == "serpis", url.host() == "pair" { app.pair(url.absoluteString, label: UIDevice.current.name) }
            }
            .sheet(isPresented: Binding(get: { app.pairingShown && app.pairing != nil }, set: { if !$0 { app.dismissPairing() } })) {
                PairingView().environment(app)
            }
            // Back in front: machines dropped meanwhile are reached again.
            .onChange(of: phase) { _, now in
                if now == .active { Task { await app.refresh() } }
            }
        }
    }

    /// Where the app keeps its machines: its own defaults, or a fresh suite for UI tests
    /// (`-SerpisMachines <JSON>` gives them theirs).
    static func store() -> UserDefaults {
        let args = ProcessInfo.processInfo.arguments
        guard let i = args.firstIndex(of: "-SerpisMachines"), i + 1 < args.count else { return .standard }
        let suite = UserDefaults(suiteName: "serpis-ui-tests")!
        suite.removePersistentDomain(forName: "serpis-ui-tests")
        suite.set(Data(args[i + 1].utf8), forKey: "machines")
        return suite
    }
}

extension ChatModel: Hashable {
    nonisolated static func == (a: ChatModel, b: ChatModel) -> Bool { a === b }
    nonisolated func hash(into h: inout Hasher) { h.combine(ObjectIdentifier(self)) }
}
