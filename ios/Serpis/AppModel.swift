import Foundation
import Observation

/// A machine the person added: what they call it, and how it is reached.
struct Machine: Codable, Hashable, Identifiable {
    var id: String
    var name: String
    /// `user@host[:port]`.
    var address: String
}

/// How the connection to a machine stands.
enum Link: Equatable {
    case offline(String?)
    case connecting
    case online(home: String)
    /// Its host key needs the person's word: new, or changed from the one accepted.
    case unknownHost(fingerprint: String, changed: Bool)

    var isOnline: Bool { if case .online = self { true } else { false } }
}

/// How pairing with a desktop (its `phone-pair` code) is going.
enum Pairing: Equatable {
    /// Waiting for the desktop's yes; it asks about this key.
    case waiting(fingerprint: String)
    /// Let in on these machines.
    case paired([String])
    case failed(String)

    var isWaiting: Bool { if case .waiting = self { true } else { false } }
}

/// A row of the chats list: a chat open on a machine, or one saved there.
struct ChatRow: Identifiable, Hashable {
    var machine: String
    /// The chat's id (its session's), open or not.
    var chat: String
    var title: String
    var cwd: String
    var modified: Date
    var running: Bool
    var step: String?
    /// Stopped at a usage limit (Claude Code's): which, and when it goes on by itself.
    var limit: Limit?
    var continuesAt: Date?
    /// The session file there, to open it again when it isn't open.
    var path: String?
    var id: String { machine + "/" + chat }

    /// The folder, as short as it reads well: `~/code/serpis` → `serpis`.
    var folder: String { (cwd as NSString).lastPathComponent }
}

/// Everything the app knows: its machines, how each is reached, the chats there, and the open
/// chats' transcripts. Lives on the main actor; the core's events hop over to it.
@MainActor
@Observable
final class AppModel {
    private(set) var machines: [Machine] = []
    private(set) var links: [String: Link] = [:]
    private(set) var rows: [ChatRow] = []
    private(set) var loading = false
    /// Why the core couldn't start, when it couldn't.
    private(set) var broken: String?
    /// Pairing with a desktop, while it goes on and once it is done.
    private(set) var pairing: Pairing?
    /// Whether how pairing goes is on screen (hidden while it waits, it shows again when done).
    var pairingShown = false

    @ObservationIgnored private var core: Core?
    @ObservationIgnored private var chats: [String: ChatModel] = [:]
    @ObservationIgnored private var listed: [String: [ChatRow]] = [:]
    @ObservationIgnored private let store: UserDefaults
    /// Connections being made, by machine: whoever asks meanwhile waits for the same one.
    @ObservationIgnored private var connecting: [String: Task<Void, Never>] = [:]

    init(store: UserDefaults = .standard) {
        self.store = store
        machines = (try? JSONDecoder().decode([Machine].self, from: store.data(forKey: "machines") ?? Data())) ?? []
        // Under UI tests (they name the machines), a fresh place each run: nothing accepted before.
        // (`-SerpisData <dir>` keeps it across launches of one test.)
        let args = ProcessInfo.processInfo.arguments
        let given = args.firstIndex(of: "-SerpisData").flatMap { args.indices.contains($0 + 1) ? URL(fileURLWithPath: args[$0 + 1]) : nil }
        let data = given
            ?? (args.contains("-SerpisMachines")
                ? FileManager.default.temporaryDirectory.appendingPathComponent("serpis-ui-\(UUID().uuidString)")
                : FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0].appendingPathComponent("core"))
        do {
            // UI tests give the app a key of theirs (`-SerpisDeviceKey <file>`), kept nowhere.
            let testKey = args.firstIndex(of: "-SerpisDeviceKey").flatMap { args.indices.contains($0 + 1) ? try? String(contentsOfFile: args[$0 + 1], encoding: .utf8) : nil }
            let core = try Core(dataDir: data.path, key: testKey ?? DeviceKey.load(), listener: Bridge(self))
            if testKey == nil, DeviceKey.load() == nil { DeviceKey.save(core.privateKey()) }
            self.core = core
        } catch {
            broken = "\(error)"
        }
    }

    /// The line a machine's `~/.ssh/authorized_keys` needs to let this device in.
    var publicKey: String { core?.publicKey() ?? "" }

    func link(_ machine: String) -> Link { links[machine] ?? .offline(nil) }

    /// `path` on `machine` as its person reads it: their home there as `~`.
    func tilde(_ path: String, on machine: String) -> String {
        guard case .online(let home) = link(machine), !home.isEmpty else { return path }
        if path == home { return "~" }
        return path.hasPrefix(home + "/") ? "~" + path.dropFirst(home.count) : path
    }

    func machine(_ id: String) -> Machine? { machines.first { $0.id == id } }

    // MARK: Machines

    func add(name: String, address: String) -> Machine {
        let m = Machine(id: UUID().uuidString, name: name.isEmpty ? address : name, address: address)
        machines.append(m)
        save()
        return m
    }

    func remove(_ m: Machine) {
        machines.removeAll { $0.id == m.id }
        core?.disconnect(machine: m.id)
        links[m.id] = nil
        listed[m.id] = nil
        rebuildRows()
        save()
    }

    /// Pair with the desktop that showed `link`: this phone is called `label` there. Its
    /// machines are added, trusted as the desktop trusts them, and connected once it says yes.
    func pair(_ link: String, label: String) {
        guard let core, pairing?.isWaiting != true else { return }
        pairing = .waiting(fingerprint: core.fingerprint())
        pairingShown = true
        Task {
            do {
                struct Shared: Decodable { let name: String; let address: String }
                let shared = try JSONDecoder().decode([Shared].self, from: Data(try await core.pair(link: link, label: label).utf8))
                let added = shared.map { s in machines.first { $0.address == s.address } ?? add(name: s.name, address: s.address) }
                pairing = .paired(added.map(\.name))
                pairingShown = true
                for m in added { await connect(m) }
                await refresh()
            } catch {
                pairing = .failed(Self.describe(error))
                pairingShown = true
            }
        }
    }

    /// Done looking at how pairing went (or hidden while it waits: it shows again when done).
    func dismissPairing() {
        pairingShown = false
        if pairing?.isWaiting == false { pairing = nil }
    }

    private func save() {
        store.set(try? JSONEncoder().encode(machines), forKey: "machines")
    }

    /// Trust the host key a machine showed, and connect.
    func accept(_ m: Machine, fingerprint: String) async {
        try? core?.acceptHost(address: m.address, fingerprint: fingerprint)
        await connect(m)
    }

    /// Connect `m` unless it is connected (or being connected: then wait for that).
    func connect(_ m: Machine) async {
        guard let core else { return }
        if core.isConnected(machine: m.id), link(m.id).isOnline { return }
        if let running = connecting[m.id] { return await running.value }
        let task = Task { await self.connectNow(m, core) }
        connecting[m.id] = task
        await task.value
        connecting[m.id] = nil
    }

    private func connectNow(_ m: Machine, _ core: Core) async {
        links[m.id] = .connecting
        do {
            let hello = try JSONDecoder().decode(Hello.self, from: Data(try await core.connect(machine: m.id, address: m.address).utf8))
            links[m.id] = .online(home: hello.home)
            // Shown again after a drop: the chats this app shows follow it again.
            for chat in chats.values where chat.machine == m.id { await chat.attach() }
        } catch let MobileError.NewHost(_, fingerprint) {
            links[m.id] = .unknownHost(fingerprint: fingerprint, changed: false)
        } catch let MobileError.HostKeyChanged(_, _, now) {
            links[m.id] = .unknownHost(fingerprint: now, changed: true)
        } catch {
            links[m.id] = .offline(Self.describe(error))
        }
    }

    /// Words for a failure, for the person.
    static func describe(_ error: Error) -> String {
        switch error {
        case MobileError.KeyRefused(let host): "\(host) didn't let this device in: add its key to ~/.ssh/authorized_keys there"
        case MobileError.NoNode(let message), MobileError.Failed(let message): message
        default: "\(error)"
        }
    }

    func closed(_ machine: String, reason: String) {
        if links[machine]?.isOnline == true {
            links[machine] = .offline(reason.isEmpty ? nil : reason)
        }
        for chat in chats.values where chat.machine == machine { chat.connected = false }
    }

    // MARK: Calls

    /// Send `machine` a request; its answer.
    func call(_ machine: String, _ request: [String: Any]) async throws -> Answer {
        guard let core else { throw MobileError.Failed(message: "The app couldn't start its connection core.") }
        let body = String(data: try JSONSerialization.data(withJSONObject: request), encoding: .utf8) ?? "{}"
        let answer = try await core.call(machine: machine, request: body)
        return try JSONDecoder().decode(Answer.self, from: Data(answer.utf8))
    }

    // MARK: The chats list

    /// Connect every machine and ask each for its chats, merging them as they answer.
    func refresh() async {
        loading = true
        await withTaskGroup(of: Void.self) { group in
            for m in machines {
                group.addTask { await self.refresh(m) }
            }
        }
        loading = false
    }

    private func refresh(_ m: Machine, again: Bool = true) async {
        await connect(m)
        guard link(m.id).isOnline else { return }
        do {
            async let open = call(m.id, ["op": "list"])
            async let saved = call(m.id, ["op": "sessions", "limit": 200])
            let (o, s) = try await (open, saved)
            var rows: [String: ChatRow] = [:]
            for session in s.sessions ?? [] where session.movedTo == nil {
                rows[session.id] = ChatRow(machine: m.id, chat: session.id, title: session.title, cwd: session.cwd, modified: Date(timeIntervalSince1970: session.modified / 1000), running: session.running, path: session.path)
            }
            for c in o.chats ?? [] {
                // Open, but not among the saved ones (nothing said yet): when is unknown, so last.
                var row = rows[c.chat] ?? ChatRow(machine: m.id, chat: c.chat, title: c.state.title, cwd: c.state.cwd, modified: .distantPast, running: false)
                row.running = c.state.running
                row.step = c.state.step
                row.limit = c.state.limit.flatMap { $0.reached && !c.state.running ? $0 : nil }
                row.continuesAt = c.state.continuesAt
                if !c.state.title.isEmpty { row.title = c.state.title }
                rows[c.chat] = row
            }
            listed[m.id] = Array(rows.values)
            rebuildRows()
        } catch {
            // A connection that looked open but wasn't (the phone slept, the network changed):
            // connected afresh, once.
            if again {
                core?.disconnect(machine: m.id)
                links[m.id] = .offline(nil)
                return await refresh(m, again: false)
            }
            links[m.id] = .offline(Self.describe(error))
        }
    }

    private func rebuildRows() {
        rows = listed.values.joined().filter { !$0.title.isEmpty }.sorted { a, b in
            if a.running != b.running { return a.running }
            return a.modified > b.modified
        }
    }

    // MARK: Chats

    /// The chat `chat` of `machine`, followed live while it is shown.
    func chat(_ machine: String, _ chat: String, path: String? = nil) -> ChatModel {
        let key = machine + "/" + chat
        if let c = chats[key] { return c }
        let c = ChatModel(app: self, machine: machine, chat: chat, path: path)
        chats[key] = c
        return c
    }

    /// A new chat on `machine` in `cwd` there.
    func newChat(on machine: String, in cwd: String) async throws -> ChatModel {
        let answer = try await call(machine, ["op": "new", "cwd": cwd])
        guard let id = answer.chat else { throw MobileError.Failed(message: "The machine didn't start a chat.") }
        return chat(machine, id)
    }

    /// The folders in `path` on `machine` (`~` is its home).
    func folders(on machine: String, in path: String) async throws -> (path: String, dirs: [String]) {
        let a = try await call(machine, ["op": "dirs", "path": path])
        return (a.path ?? path, a.dirs ?? [])
    }

    /// Folders chats on `machine` ran in, most recent first.
    func recentFolders(on machine: String) -> [String] {
        var seen = Set<String>()
        return (listed[machine] ?? []).sorted { $0.modified > $1.modified }.map(\.cwd).filter { !$0.isEmpty && seen.insert($0).inserted }
    }

    func forget(_ chat: ChatModel) {
        chats[chat.machine + "/" + chat.chat] = nil
    }

    fileprivate func heard(_ machine: String, _ event: Event) {
        let chat: String
        switch event {
        case .splice(let c, _), .state(let c, _), .closed(let c), .notice(let c, _, _): chat = c
        case .other: return
        }
        chats[machine + "/" + chat]?.take(event)
        if case .state(_, let s) = event, let i = rows.firstIndex(where: { $0.machine == machine && $0.chat == chat }) {
            rows[i].running = s.running
            rows[i].step = s.step
            if !s.title.isEmpty { rows[i].title = s.title }
        }
    }
}

/// What the core says, taken to the main actor.
private final class Bridge: Listener, @unchecked Sendable {
    weak var app: AppModel?

    @MainActor init(_ app: AppModel) { self.app = app }

    func event(machine: String, event: String) {
        // Read here, off the main thread: a long chat's transcript is a lot of JSON.
        guard let decoded = try? JSONDecoder().decode(Event.self, from: Data(event.utf8)) else { return }
        Task { @MainActor [weak app] in app?.heard(machine, decoded) }
    }

    func closed(machine: String, reason: String) {
        Task { @MainActor [weak app] in app?.closed(machine, reason: reason) }
    }
}
