import Foundation

// What nodes say, as the protocol spells it (crates/serpis-proto): decoded from the JSON the
// core hands over. Only what the app shows is kept; the rest of each message is skipped.

/// Any JSON value (a tool's arguments).
enum JSON: Decodable, Hashable {
    case string(String), number(Double), bool(Bool), array([JSON]), object([String: JSON]), null

    init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { self = .null }
        else if let v = try? c.decode(Bool.self) { self = .bool(v) }
        else if let v = try? c.decode(Double.self) { self = .number(v) }
        else if let v = try? c.decode(String.self) { self = .string(v) }
        else if let v = try? c.decode([JSON].self) { self = .array(v) }
        else { self = .object(try c.decode([String: JSON].self)) }
    }

    subscript(key: String) -> JSON? {
        if case .object(let o) = self { return o[key] }
        return nil
    }

    var string: String? {
        if case .string(let s) = self { return s }
        return nil
    }
}

/// A block of an assistant's response.
enum Block: Decodable, Hashable {
    case text(String)
    case thinking(String)
    case toolCall(id: String, name: String)

    private enum Keys: String, CodingKey { case type, text, thinking, id, name }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .type) {
        case "text": self = .text(try c.decode(String.self, forKey: .text))
        case "thinking": self = .thinking((try? c.decode(String.self, forKey: .thinking)) ?? "")
        default: self = .toolCall(id: (try? c.decode(String.self, forKey: .id)) ?? "", name: (try? c.decode(String.self, forKey: .name)) ?? "")
        }
    }
}

struct ToolResult: Decodable, Hashable {
    var text: String
    var isError: Bool
    /// An edit's or a write's change, one line each: `+12 added`, `-12 removed`, ` 12 kept`.
    var diff: String?

    private enum Keys: String, CodingKey { case content, isError, details }
    private struct Part: Decodable { var type: String; var text: String? }
    private struct Details: Decodable { var diff: String? }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        let parts = (try? c.decode([Part].self, forKey: .content)) ?? []
        text = parts.compactMap(\.text).joined(separator: "\n")
        isError = (try? c.decode(Bool.self, forKey: .isError)) ?? false
        diff = (try? c.decode(Details.self, forKey: .details))?.diff
    }
}

struct SentImage: Codable, Hashable {
    var mime: String
    var data: String
}

/// A tool call waiting for the person to allow it; `always` says what allowing it from now on
/// would cover, when that's offered.
struct Ask: Decodable, Hashable {
    var always: String?
}

/// Work a tool call started on the side: a command in the background, a subagent.
struct TaskInfo: Decodable, Hashable {
    var id: String
    /// "bash" or "agent".
    var kind: String
    var description: String
    /// "running", "completed", "failed" or "stopped".
    var status: String
    var background: Bool
    var summary: String?
    var lastTool: String?
    var toolUses: Int
    var tokens: Int

    private enum Keys: String, CodingKey { case id, kind, description, status, background, summary, last_tool, tool_uses, tokens }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        id = (try? c.decode(String.self, forKey: .id)) ?? ""
        kind = (try? c.decode(String.self, forKey: .kind)) ?? "bash"
        description = (try? c.decode(String.self, forKey: .description)) ?? ""
        status = (try? c.decode(String.self, forKey: .status)) ?? "running"
        background = (try? c.decode(Bool.self, forKey: .background)) ?? false
        summary = try? c.decode(String.self, forKey: .summary)
        lastTool = try? c.decode(String.self, forKey: .last_tool)
        toolUses = (try? c.decode(Int.self, forKey: .tool_uses)) ?? 0
        tokens = (try? c.decode(Int.self, forKey: .tokens)) ?? 0
    }

    var running: Bool { status == "running" }

    /// How it is going, in a few words: "In the background", "2 tools · 1.2k tokens".
    var statusLine: String {
        var parts: [String] = []
        switch status {
        case "running": if background { parts.append("In the background") }
        case "failed": parts.append("Failed")
        case "stopped": parts.append("Stopped")
        default: if background { parts.append("Done in the background") }
        }
        if toolUses > 0 { parts.append(toolUses == 1 ? "1 tool" : "\(toolUses) tools") }
        if tokens > 0 { parts.append(tokens >= 1000 ? String(format: "%.1fk tokens", Double(tokens) / 1000) : "\(tokens) tokens") }
        return parts.joined(separator: " · ")
    }
}

/// What a tool call started on the side: the work, and a subagent's own steps.
struct Side: Hashable {
    var task: TaskInfo? = nil
    var steps: [Entry] = []
}

/// One item of a chat's transcript.
enum Entry: Decodable, Hashable {
    case user(text: String, queued: Bool, images: [SentImage])
    case assistant(blocks: [Block], streaming: Bool, error: String?)
    case tool(id: String, name: String, args: JSON, partial: String?, result: ToolResult?, elapsedMs: Int?, ask: Ask?, side: Side)
    case info(text: String, tone: String)

    private enum Keys: String, CodingKey { case kind, text, queued, images, blocks, streaming, error, id, name, args, partial, result, tone, elapsed_ms, ask, task, steps }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "user":
            self = .user(text: try c.decode(String.self, forKey: .text), queued: (try? c.decode(Bool.self, forKey: .queued)) ?? false, images: (try? c.decode([SentImage].self, forKey: .images)) ?? [])
        case "assistant":
            self = .assistant(blocks: try c.decode([Block].self, forKey: .blocks), streaming: (try? c.decode(Bool.self, forKey: .streaming)) ?? false, error: try? c.decode(String.self, forKey: .error))
        case "tool":
            self = .tool(id: try c.decode(String.self, forKey: .id), name: try c.decode(String.self, forKey: .name), args: (try? c.decode(JSON.self, forKey: .args)) ?? .null, partial: try? c.decode(String.self, forKey: .partial), result: try? c.decode(ToolResult.self, forKey: .result), elapsedMs: try? c.decode(Int.self, forKey: .elapsed_ms), ask: try? c.decode(Ask.self, forKey: .ask), side: Side(task: try? c.decode(TaskInfo.self, forKey: .task), steps: (try? c.decode([Entry].self, forKey: .steps)) ?? []))
        default:
            self = .info(text: (try? c.decode(String.self, forKey: .text)) ?? "", tone: (try? c.decode(String.self, forKey: .tone)) ?? "meta")
        }
    }
}

struct Splice: Decodable {
    var from: Int
    var entries: [Entry]

    func apply(to transcript: inout [Entry]) {
        transcript.removeSubrange(min(from, transcript.count)...)
        transcript.append(contentsOf: entries)
    }
}

/// A chat's state around its transcript.
struct ChatState: Decodable, Hashable {
    var title: String
    var model: String?
    var running: Bool
    var step: String?
    var cwd: String
    var sessionId: String
    var lastError: String?
    /// Claude Code's permission mode, for a chat on it.
    var permissionMode: String?

    private enum Keys: String, CodingKey { case title, model, running, step, cwd, session_id, last_error, permission_mode }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        title = (try? c.decode(String.self, forKey: .title)) ?? ""
        model = try? c.decode(String.self, forKey: .model)
        running = (try? c.decode(Bool.self, forKey: .running)) ?? false
        step = try? c.decode(String.self, forKey: .step)
        cwd = (try? c.decode(String.self, forKey: .cwd)) ?? ""
        sessionId = (try? c.decode(String.self, forKey: .session_id)) ?? ""
        lastError = try? c.decode(String.self, forKey: .last_error)
        permissionMode = try? c.decode(String.self, forKey: .permission_mode)
    }
}

/// Claude Code's permission modes: (mode, what its prompt line says, what it does).
let permissionModes: [(mode: String, label: String, about: String)] = [
    ("default", "Asks before acting", "Asks before edits and commands its rules don't allow"),
    ("acceptEdits", "⏵⏵ accept edits on", "Edits without asking; asks for commands"),
    ("plan", "⏸ plan mode on", "Reads and plans; changes nothing until you approve"),
    ("auto", "⏵⏵ auto mode on", "Decides what is safe; asks only for what isn't"),
    ("dontAsk", "⏵⏵ don't ask on", "Never asks: what its rules don't allow is refused"),
    ("bypassPermissions", "⏵⏵ bypass permissions on", "Never asks and allows everything"),
]

/// Something a machine said about a chat it runs.
enum Event: Decodable {
    case splice(chat: String, Splice)
    case state(chat: String, ChatState)
    case closed(chat: String)
    case notice(chat: String, text: String, level: String)
    case other

    private enum Keys: String, CodingKey { case kind, chat, splice, state, text, level }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        let chat = (try? c.decode(String.self, forKey: .chat)) ?? ""
        switch try c.decode(String.self, forKey: .kind) {
        case "splice": self = .splice(chat: chat, try c.decode(Splice.self, forKey: .splice))
        case "state": self = .state(chat: chat, try c.decode(ChatState.self, forKey: .state))
        case "closed": self = .closed(chat: chat)
        case "notice": self = .notice(chat: chat, text: (try? c.decode(String.self, forKey: .text)) ?? "", level: (try? c.decode(String.self, forKey: .level)) ?? "info")
        default: self = .other
        }
    }
}

/// A chat open on a machine now.
struct ChatSummary: Decodable {
    var chat: String
    var state: ChatState
}

/// A saved chat on a machine.
struct SessionInfo: Decodable, Hashable {
    var path: String
    var id: String
    var title: String
    var cwd: String
    /// Milliseconds since the epoch.
    var modified: Double
    var messages: Int
    var movedTo: String?
    var open: Bool
    var running: Bool

    private enum Keys: String, CodingKey { case path, id, title, cwd, modified, messages, moved_to, open, running }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        path = try c.decode(String.self, forKey: .path)
        id = try c.decode(String.self, forKey: .id)
        title = (try? c.decode(String.self, forKey: .title)) ?? ""
        cwd = (try? c.decode(String.self, forKey: .cwd)) ?? ""
        modified = (try? c.decode(Double.self, forKey: .modified)) ?? 0
        messages = (try? c.decode(Int.self, forKey: .messages)) ?? 0
        movedTo = try? c.decode(String.self, forKey: .moved_to)
        open = (try? c.decode(Bool.self, forKey: .open)) ?? false
        running = (try? c.decode(Bool.self, forKey: .running)) ?? false
    }
}

struct ModelInfo: Decodable, Hashable, Identifiable {
    var model: String
    var name: String
    var images: Bool
    var id: String { model }
}

/// A node's answer: only the fields the app reads, by the answer's kind.
struct Answer: Decodable {
    var kind: String
    var chat: String?
    var chats: [ChatSummary]?
    var sessions: [SessionInfo]?
    var models: [ModelInfo]?
    var path: String?
    var dirs: [String]?
}

/// The node's hello, as the core returns it.
struct Hello: Decodable {
    var host: String
    var home: String
}
