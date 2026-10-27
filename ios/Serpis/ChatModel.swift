import Foundation
import Observation

/// One chat on a machine, followed live while it is shown: its transcript, kept from the splices
/// its node sends, and its state. What is sent goes straight to the node; the transcript only
/// ever changes as the node says.
@MainActor
@Observable
final class ChatModel: Identifiable {
    let machine: String
    private(set) var chat: String
    private(set) var entries: [Entry] = []
    private(set) var state: ChatState?
    /// Attached now: what happens there shows here as it happens.
    var connected = false
    /// Something to tell the person (a refused message, a failed connection), until they read it.
    var notice: String?
    /// Models the machine has, once asked.
    private(set) var models: [ModelInfo] = []

    @ObservationIgnored private weak var app: AppModel?
    /// The session file, to open the chat again when its machine closed it meanwhile.
    @ObservationIgnored private var path: String?
    @ObservationIgnored private var requests = 0

    init(app: AppModel, machine: String, chat: String, path: String?) {
        self.app = app
        self.machine = machine
        self.chat = chat
        self.path = path
    }

    var running: Bool { state?.running ?? false }
    var title: String { state?.title.isEmpty == false ? state!.title : "New chat" }
    var model: String? { state?.model }
    var modelName: String {
        guard let m = model else { return "Model" }
        return models.first { $0.model == m }?.name ?? String(m.split(separator: "/").last ?? Substring(m))
    }

    /// Follow the chat: opened again from its session when its machine had closed it.
    func attach() async {
        guard let app else { return }
        do {
            _ = try await app.call(machine, ["op": "attach", "chat": chat])
            connected = true
        } catch {
            if let path {
                do {
                    let a = try await app.call(machine, ["op": "resume", "session": path])
                    if let id = a.chat { chat = id }
                    _ = try await app.call(machine, ["op": "attach", "chat": chat])
                    connected = true
                } catch {
                    notice = AppModel.describe(error)
                }
            } else {
                notice = AppModel.describe(error)
            }
        }
        if models.isEmpty, let a = try? await app.call(machine, ["op": "models"]) {
            models = a.models ?? []
        }
    }

    /// Stop following it. A chat at rest that nobody else shows is closed there, its session
    /// kept; one at work keeps working.
    func detach() {
        guard let app, connected else { return }
        connected = false
        let (m, c) = (machine, chat)
        Task { _ = try? await app.call(m, ["op": "detach", "chat": c, "close": true]) }
        app.forget(self)
    }

    /// Send a message (while it works, it steers the turn).
    func send(_ text: String, images: [SentImage]) async -> Bool {
        guard let app else { return false }
        requests += 1
        let request: [String: Any] = [
            "op": "prompt", "chat": chat, "text": text, "follow_up": false,
            "images": images.map { ["mime": $0.mime, "data": $0.data] },
            // A retry of a message whose answer was lost doesn't send it twice.
            "request_id": "\(chat)-\(Date().timeIntervalSince1970)-\(requests)",
        ]
        do {
            _ = try await app.call(machine, request)
            return true
        } catch {
            notice = AppModel.describe(error)
            return false
        }
    }

    func stop() async {
        _ = try? await app?.call(machine, ["op": "abort", "chat": chat])
    }

    /// The first tool call waiting for the person to allow it.
    var waiting: Tool? {
        entries.lazy.compactMap(Tool.init).first { $0.ask != nil }
    }

    /// Tool calls whose work runs on in the background (Claude Code's commands, agents).
    var background: [Tool] {
        entries.compactMap(Tool.init).filter(\.working)
    }

    /// Switch Claude Code's permission mode for this chat.
    func setPermissionMode(_ mode: String) async {
        do {
            _ = try await app?.call(machine, ["op": "set_permission_mode", "chat": chat, "mode": mode])
        } catch {
            notice = AppModel.describe(error)
        }
    }

    /// Ask the agent to stop the work `tool` started.
    func stopTask(_ tool: Tool) async {
        guard let task = tool.side.task?.id else { return }
        do {
            _ = try await app?.call(machine, ["op": "stop_task", "chat": chat, "task": task])
        } catch {
            notice = AppModel.describe(error)
        }
    }

    /// Answer what `tool` asks: "allow", "always", "deny" (with `message`), "answers" (its
    /// questions, to `answers`) or "plan" (approved, going on in `mode`).
    func answer(_ tool: String, _ kind: String, answers: [String: String]? = nil, mode: String? = nil, message: String? = nil) async {
        var decision: [String: Any] = ["kind": kind]
        if let answers { decision["answers"] = answers }
        if let mode { decision["mode"] = mode }
        if let message { decision["message"] = message }
        do {
            _ = try await app?.call(machine, ["op": "answer", "chat": chat, "tool": tool, "decision": decision])
        } catch {
            notice = AppModel.describe(error)
        }
    }

    func setModel(_ model: String) async {
        do {
            _ = try await app?.call(machine, ["op": "set_model", "chat": chat, "model": model])
        } catch {
            notice = AppModel.describe(error)
        }
    }

    func take(_ event: Event) {
        switch event {
        case .splice(_, let s): s.apply(to: &entries)
        case .state(_, let s): state = s
        case .closed: connected = false
        case .notice(_, let text, _): notice = text
        case .other: break
        }
    }
}
