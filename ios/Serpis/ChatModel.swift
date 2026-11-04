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
    private(set) var entries: [Entry] = [] { didSet { derive() } }
    /// What the transcript shows, and what its tool calls add up to: worked out once per change.
    private(set) var items: [Item] = []
    /// The first tool call waiting for the person to allow it.
    private(set) var waiting: Tool?
    /// Tool calls whose work runs on in the background (Claude Code's commands, agents).
    private(set) var background: [Tool] = []
    /// What it waits on now: a command or subagent that can move to the background.
    private(set) var foreground: [Tool] = []
    /// Goes up whenever the transcript changes.
    private(set) var revision = 0
    private(set) var state: ChatState?
    /// Its transcript hasn't come yet, the first time it is asked for.
    private(set) var loading = true
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
    var title: String { state?.title.isEmpty == false ? state!.title : loading ? "" : "New chat" }
    var model: String? { state?.model }
    var modelName: String {
        guard let m = model else { return "Model" }
        return models.first { $0.model == m }?.name ?? String(m.split(separator: "/").last ?? Substring(m))
    }

    /// The thinking levels its model takes; none when it doesn't think.
    var thinkingLevels: [String] { models.first { $0.model == model }?.thinkingLevels ?? [] }

    /// Follow the chat: opened again from its session when its machine had closed it.
    func attach() async {
        guard let app else { return }
        // (Attached, its transcript comes as a splice: `take` ends this.)
        loading = entries.isEmpty
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
                    loading = false
                }
            } else {
                notice = AppModel.describe(error)
                loading = false
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

    private func derive() {
        revision += 1
        items = Item.of(entries)
        let tools = entries.compactMap(Tool.init)
        waiting = tools.first { $0.ask != nil }
        background = tools.filter { $0.working && $0.side.task?.background == true }
        foreground = tools.filter { $0.result == nil && $0.side.task?.running == true && $0.side.task?.background == false }
    }

    /// Move what `tool` runs to the background (Claude Code's Ctrl+B): the turn goes on.
    func backgroundTask(_ tool: Tool) async {
        do {
            _ = try await app?.call(machine, ["op": "background_task", "chat": chat, "tool": tool.id])
        } catch {
            notice = AppModel.describe(error)
        }
    }

    /// Switch Claude Code's permission mode for this chat.
    func setPermissionMode(_ mode: String) async {
        do {
            _ = try await app?.call(machine, ["op": "set_permission_mode", "chat": chat, "mode": mode])
        } catch {
            notice = AppModel.describe(error)
        }
    }

    /// Whether the chat, stopped at a usage limit, goes on by itself when it resets.
    func setContinueAfterLimit(_ on: Bool) async {
        do {
            _ = try await app?.call(machine, ["op": "set_continue_after_limit", "chat": chat, "on": on])
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

    func setThinking(_ level: String) async {
        do {
            _ = try await app?.call(machine, ["op": "set_thinking", "chat": chat, "level": level])
        } catch {
            notice = AppModel.describe(error)
        }
    }

    func take(_ event: Event) {
        switch event {
        case .splice(_, let s):
            s.apply(to: &entries)
            loading = false
        case .state(_, let s): state = s
        case .closed: connected = false
        case .notice(_, let text, _): notice = text
        case .other: break
        }
    }
}
