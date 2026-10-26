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

/// One item of a chat's transcript.
enum Entry: Decodable, Hashable {
    case user(text: String, queued: Bool, images: [SentImage])
    case assistant(blocks: [Block], streaming: Bool, error: String?)
    case tool(id: String, name: String, args: JSON, partial: String?, result: ToolResult?, elapsedMs: Int?)
    case info(text: String, tone: String)

    private enum Keys: String, CodingKey { case kind, text, queued, images, blocks, streaming, error, id, name, args, partial, result, tone, elapsed_ms }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "user":
            self = .user(text: try c.decode(String.self, forKey: .text), queued: (try? c.decode(Bool.self, forKey: .queued)) ?? false, images: (try? c.decode([SentImage].self, forKey: .images)) ?? [])
        case "assistant":
            self = .assistant(blocks: try c.decode([Block].self, forKey: .blocks), streaming: (try? c.decode(Bool.self, forKey: .streaming)) ?? false, error: try? c.decode(String.self, forKey: .error))
        case "tool":
            self = .tool(id: try c.decode(String.self, forKey: .id), name: try c.decode(String.self, forKey: .name), args: (try? c.decode(JSON.self, forKey: .args)) ?? .null, partial: try? c.decode(String.self, forKey: .partial), result: try? c.decode(ToolResult.self, forKey: .result), elapsedMs: try? c.decode(Int.self, forKey: .elapsed_ms))
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

    private enum Keys: String, CodingKey { case title, model, running, step, cwd, session_id, last_error }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        title = (try? c.decode(String.self, forKey: .title)) ?? ""
        model = try? c.decode(String.self, forKey: .model)
        running = (try? c.decode(Bool.self, forKey: .running)) ?? false
        step = try? c.decode(String.self, forKey: .step)
        cwd = (try? c.decode(String.self, forKey: .cwd)) ?? ""
        sessionId = (try? c.decode(String.self, forKey: .session_id)) ?? ""
        lastError = try? c.decode(String.self, forKey: .last_error)
    }
}

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
