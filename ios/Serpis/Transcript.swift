import SwiftUI

// How a chat's transcript reads: your messages in bubbles on the right, the agent's answers as
// text across the screen, and what it did between them folded into one quiet block per stretch
// of work: open while it works, one line when it's done, each step a tap away from what it did
// (a command and its output, a file, a diff).

/// What the transcript shows, in order: entries, with each stretch of work (tool calls, and the
/// responses between them that only called tools or thought) gathered into one.
enum Item: Identifiable {
    case entry(Int, Entry)
    case work(Int, [Entry])

    var id: Int {
        switch self {
        case .entry(let i, _), .work(let i, _): i
        }
    }

    static func of(_ entries: [Entry]) -> [Item] {
        var out: [Item] = []
        var run: [Entry] = []
        var start = 0
        for (i, e) in entries.enumerated() {
            if e.isWork {
                if run.isEmpty { start = i }
                run.append(e)
                continue
            }
            if !run.isEmpty {
                out.append(run.contains { if case .tool = $0 { true } else { false } } ? .work(start, run) : .entry(start, run[0]))
                run = []
            }
            out.append(.entry(i, e))
        }
        if !run.isEmpty {
            out.append(run.contains { if case .tool = $0 { true } else { false } } ? .work(start, run) : .entry(start, run[0]))
        }
        return out
    }
}

extension Entry {
    /// Part of a stretch of work: a tool call, or a response that said nothing (it only called
    /// tools, or only thought).
    var isWork: Bool {
        switch self {
        case .tool: return true
        case .assistant(let blocks, _, let error):
            return error == nil && !blocks.contains { if case .text(let t) = $0 { !t.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty } else { false } }
        default: return false
        }
    }
}

/// One entry that isn't work.
struct EntryView: View {
    let entry: Entry

    var body: some View {
        switch entry {
        case .user(let text, let queued, let images):
            UserBubble(text: text, queued: queued, images: images)
        case .assistant(let blocks, let streaming, let error):
            AssistantView(blocks: blocks, streaming: streaming, error: error)
        case .tool:
            EmptyView()
        case .info(let text, let tone):
            Text(text)
                .font(.footnote)
                .foregroundStyle(tone == "warning" || tone == "error" ? AnyShapeStyle(.orange) : AnyShapeStyle(.secondary))
                .multilineTextAlignment(.center)
                .frame(maxWidth: .infinity)
                .padding(.horizontal, 24)
        }
    }
}

struct UserBubble: View {
    let text: String
    let queued: Bool
    let images: [SentImage]

    var body: some View {
        VStack(alignment: .trailing, spacing: 6) {
            ForEach(images, id: \.self) { image in
                if let data = Data(base64Encoded: image.data), let ui = UIImage(data: data) {
                    Image(uiImage: ui)
                        .resizable()
                        .scaledToFill()
                        .frame(maxWidth: 220, maxHeight: 220)
                        .clipShape(RoundedRectangle(cornerRadius: 18, style: .continuous))
                }
            }
            if !text.isEmpty {
                Text(text)
                    .font(.body)
                    .textSelection(.enabled)
                    .padding(.horizontal, 15)
                    .padding(.vertical, 10)
                    .background(Color.userBubble, in: RoundedRectangle(cornerRadius: 20, style: .continuous))
            }
            if queued {
                Label("Sent while it works", systemImage: "clock")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: .infinity, alignment: .trailing)
        .padding(.leading, 56)
    }
}

struct AssistantView: View {
    let blocks: [Block]
    let streaming: Bool
    let error: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                switch block {
                case .text(let t):
                    MarkdownView(text: t)
                case .thinking(let t) where !t.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty:
                    Thought(text: t, live: streaming && block == blocks.last)
                default:
                    EmptyView()
                }
            }
            if streaming && blocks.allSatisfy({ if case .text(let t) = $0 { t.isEmpty } else { true } }) {
                Working()
            }
            if let error {
                Label(error, systemImage: "exclamationmark.triangle.fill")
                    .font(.footnote)
                    .foregroundStyle(.orange)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// The model's reasoning, folded away.
struct Thought: View {
    let text: String
    let live: Bool
    @State private var open = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Button {
                withAnimation(.snappy) { open.toggle() }
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: "sparkle")
                        .symbolEffect(.pulse, isActive: live)
                    Text(live ? "Thinking" : "Thought")
                    Image(systemName: "chevron.right")
                        .font(.caption2.weight(.bold))
                        .rotationEffect(.degrees(open ? 90 : 0))
                }
                .font(.subheadline.weight(.medium))
                .foregroundStyle(.secondary)
            }
            .buttonStyle(.plain)
            if open {
                Text(text)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .lineSpacing(3)
                    .textSelection(.enabled)
                    .padding(.leading, 14)
                    .overlay(alignment: .leading) { Capsule().fill(.quaternary).frame(width: 2) }
                    .transition(.opacity.combined(with: .move(edge: .top)))
            }
        }
    }
}

/// The dots of a response that hasn't said anything yet.
struct Working: View {
    var body: some View {
        TimelineView(.animation) { context in
            let t = context.date.timeIntervalSinceReferenceDate
            HStack(spacing: 5) {
                ForEach(0..<3) { i in
                    Circle()
                        .frame(width: 7, height: 7)
                        .scaleEffect(0.75 + 0.25 * max(0, sin(t * 4 - Double(i) * 0.7)))
                        .opacity(0.3 + 0.7 * max(0, sin(t * 4 - Double(i) * 0.7)))
                }
            }
            .foregroundStyle(.secondary)
        }
        .padding(.vertical, 6)
        .accessibilityLabel("Working")
    }
}

// MARK: Work

/// A stretch of work: one line saying what was done, opening onto each step.
struct WorkView: View {
    let entries: [Entry]
    /// The chat is still working on this stretch.
    let live: Bool
    @State private var open: Bool?

    private var tools: [Tool] {
        entries.compactMap { if case .tool(let id, let name, let args, let partial, let result, let elapsed) = $0 { Tool(id: id, name: name, args: args, partial: partial, result: result, elapsedMs: elapsed) } else { nil } }
    }

    var body: some View {
        let tools = tools
        let isOpen = open ?? live
        VStack(alignment: .leading, spacing: 0) {
            Button {
                withAnimation(.snappy) { open = !isOpen }
            } label: {
                HStack(spacing: 8) {
                    if live, let current = tools.last(where: { $0.result == nil }) {
                        ProgressView().controlSize(.small)
                        Text(current.doing).lineLimit(1)
                    } else {
                        Text("Worked").fontWeight(.medium)
                        ForEach(Tool.kinds(of: tools), id: \.icon) { kind in
                            HStack(spacing: 3) {
                                Image(systemName: kind.icon).font(.caption2.weight(.semibold))
                                Text("\(kind.count)").font(.caption.weight(.medium).monospacedDigit())
                            }
                            .padding(.horizontal, 7)
                            .padding(.vertical, 3)
                            .background(.quaternary.opacity(0.7), in: Capsule())
                        }
                        if tools.contains(where: { $0.result?.isError == true }) {
                            Image(systemName: "exclamationmark.circle.fill").foregroundStyle(.orange).font(.caption)
                        }
                    }
                    Spacer(minLength: 4)
                    if let took = Tool.took(tools), !live {
                        Text(took).foregroundStyle(.tertiary).monospacedDigit()
                    }
                    Image(systemName: "chevron.right")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.tertiary)
                        .rotationEffect(.degrees(isOpen ? 90 : 0))
                }
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .padding(.vertical, 10)
                .padding(.horizontal, 12)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("work")
            if isOpen {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(entries.enumerated()), id: \.offset) { _, e in
                        switch e {
                        case .tool(let id, let name, let args, let partial, let result, let elapsed):
                            ToolStep(tool: Tool(id: id, name: name, args: args, partial: partial, result: result, elapsedMs: elapsed))
                        case .assistant(let blocks, let streaming, _):
                            ForEach(Array(blocks.enumerated()), id: \.offset) { _, b in
                                if case .thinking(let t) = b, !t.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                                    Thought(text: t, live: streaming).padding(.horizontal, 12).padding(.vertical, 6)
                                }
                            }
                        default:
                            EmptyView()
                        }
                    }
                }
                .padding(.bottom, 6)
                .transition(.opacity)
            }
        }
        .background(Color.workCard, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(Color.hairline, lineWidth: 0.5))
    }
}

/// A tool call, as the views read it.
struct Tool: Hashable {
    var id: String
    var name: String
    var args: JSON
    var partial: String?
    var result: ToolResult?
    var elapsedMs: Int?

    func arg(_ k: String) -> String { args[k]?.string ?? "" }

    var path: String { arg("path") }
    var file: String { (path as NSString).lastPathComponent }
    var folder: String {
        let d = (path as NSString).deletingLastPathComponent
        return d.isEmpty ? "" : (d as NSString).abbreviatingWithTildeInPath
    }
    var command: String { arg("command") }

    /// The step in a line, as it is going on.
    var doing: String {
        switch name {
        case "bash": "Running \(command.components(separatedBy: "\n").first ?? "")"
        case "read": "Reading \(file)"
        case "edit": "Editing \(file)"
        case "write": "Writing \(file)"
        case "grep", "find": "Searching for \(arg("pattern"))"
        case "ls": "Listing \(file.isEmpty ? "files" : file)"
        case "web_search": "Searching the web"
        default: "Running \(name)"
        }
    }

    /// What the stretch did, by kind, counted: commands, files read, files changed, searches.
    static func kinds(of tools: [Tool]) -> [(icon: String, count: Int)] {
        let groups: [(String, Set<String>)] = [("terminal", ["bash"]), ("doc.text", ["read"]), ("pencil.line", ["edit", "write"]), ("magnifyingglass", ["grep", "find", "ls", "web_search"])]
        var out = groups.map { g in (icon: g.0, count: tools.filter { g.1.contains($0.name) }.count) }
        let known = groups.reduce(Set<String>()) { $0.union($1.1) }
        out.append((icon: "wrench.and.screwdriver", count: tools.filter { !known.contains($0.name) }.count))
        return out.filter { $0.count > 0 }
    }

    /// What the stretch did, counted: "Ran 2 commands, edited a file".
    static func summary(of tools: [Tool]) -> String {
        func n(_ count: Int, _ one: String, _ many: String) -> String { count == 1 ? one : "\(count) \(many)" }
        var parts: [String] = []
        let count = { (names: Set<String>) in tools.filter { names.contains($0.name) }.count }
        let commands = count(["bash"]), reads = count(["read"]), edits = count(["edit", "write"]), searches = count(["grep", "find", "ls", "web_search"])
        let others = tools.count - commands - reads - edits - searches
        if commands > 0 { parts.append("ran " + n(commands, "a command", "commands")) }
        if reads > 0 { parts.append("read " + n(reads, "a file", "files")) }
        if edits > 0 { parts.append("edited " + n(edits, "a file", "files")) }
        if searches > 0 { parts.append("searched " + (searches == 1 ? "once" : "\(searches) times")) }
        if others > 0 { parts.append("used " + n(others, "a tool", "tools")) }
        let s = parts.joined(separator: ", ")
        return s.prefix(1).uppercased() + s.dropFirst()
    }

    /// How long the stretch took, when its steps say.
    static func took(_ tools: [Tool]) -> String? {
        let ms = tools.compactMap(\.elapsedMs).reduce(0, +)
        guard ms > 0 else { return nil }
        let s = Double(ms) / 1000
        return s < 10 ? String(format: "%.1fs", s) : s < 90 ? "\(Int(s))s" : "\(Int(s / 60))m \(Int(s) % 60)s"
    }
}

/// One step of a stretch of work: a line, opening onto what it did.
struct ToolStep: View {
    let tool: Tool
    @State private var open = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Button {
                withAnimation(.snappy) { open.toggle() }
            } label: {
                HStack(spacing: 10) {
                    Image(systemName: icon)
                        .font(.footnote.weight(.medium))
                        .foregroundStyle(.secondary)
                        .frame(width: 22)
                    VStack(alignment: .leading, spacing: 1) {
                        title
                        if let sub = subtitle {
                            Text(sub).font(.caption).foregroundStyle(.tertiary).lineLimit(1)
                        }
                    }
                    Spacer(minLength: 6)
                    status
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            if open {
                detail
                    .padding(.leading, 32)
                    .transition(.opacity)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 7)
    }

    @ViewBuilder private var title: some View {
        switch tool.name {
        case "bash":
            Text(tool.command.components(separatedBy: "\n").first ?? "")
                .font(.system(.subheadline, design: .monospaced))
                .lineLimit(1)
        case "read", "edit", "write", "ls":
            Text(tool.file.isEmpty ? tool.path : tool.file).font(.subheadline.weight(.medium)).lineLimit(1)
        case "grep", "find":
            (Text(tool.name == "grep" ? "Searched for " : "Found ") + Text(tool.arg("pattern")).font(.system(.subheadline, design: .monospaced)))
                .font(.subheadline).lineLimit(1)
        case "web_search":
            Text(tool.arg("query")).font(.subheadline).lineLimit(1)
        default:
            Text(tool.name).font(.subheadline.weight(.medium))
        }
    }

    private var subtitle: String? {
        switch tool.name {
        case "read":
            var s = tool.folder
            if let o = tool.args["offset"], case .number(let n) = o { s += (s.isEmpty ? "" : " · ") + "from line \(Int(n))" }
            return s.isEmpty ? nil : s
        case "edit", "write", "ls": return tool.folder.isEmpty ? nil : tool.folder
        case "grep", "find":
            let lines = tool.result?.text.split(separator: "\n").count ?? 0
            let place = tool.path.isEmpty ? "" : "in \((tool.path as NSString).abbreviatingWithTildeInPath)"
            return [place, tool.result == nil ? "" : (lines == 1 ? "1 result" : "\(lines) results")].filter { !$0.isEmpty }.joined(separator: " · ")
        default: return nil
        }
    }

    @ViewBuilder private var status: some View {
        if tool.result == nil {
            ProgressView().controlSize(.mini)
        } else if tool.result?.isError == true {
            Image(systemName: "exclamationmark.circle.fill").foregroundStyle(.orange).font(.footnote)
        } else if let d = tool.result?.diff {
            let (added, removed) = DiffView.counts(d)
            HStack(spacing: 4) {
                if added > 0 { Text("+\(added)").foregroundStyle(.green) }
                if removed > 0 { Text("−\(removed)").foregroundStyle(.red) }
            }
            .font(.caption.weight(.semibold).monospacedDigit())
        }
    }

    @ViewBuilder private var detail: some View {
        if let diff = tool.result?.diff, !diff.isEmpty {
            DiffView(diff: diff)
        } else if tool.name == "bash" {
            TerminalCard(command: tool.command, output: tool.result?.text ?? tool.partial ?? "", failed: tool.result?.isError == true)
        } else if let text = tool.result?.text ?? tool.partial, !text.isEmpty {
            OutputCard(text: text, failed: tool.result?.isError == true)
        }
    }

    private var icon: String {
        switch tool.name {
        case "bash": "terminal"
        case "read": FileIcon.of(tool.file)
        case "edit": "pencil.line"
        case "write": "square.and.pencil"
        case "grep", "find": "magnifyingglass"
        case "ls": "folder"
        case "web_search": "globe"
        default: "wrench.and.screwdriver"
        }
    }
}

enum FileIcon {
    static func of(_ file: String) -> String {
        switch (file as NSString).pathExtension.lowercased() {
        case "swift", "rs", "py", "js", "ts", "tsx", "go", "c", "h", "cpp", "java", "kt", "rb", "lua", "zig": "chevron.left.forwardslash.chevron.right"
        case "md", "txt", "rst": "doc.plaintext"
        case "json", "toml", "yaml", "yml", "xml", "plist": "curlybraces"
        case "png", "jpg", "jpeg", "gif", "webp", "svg": "photo"
        case "sh", "zsh", "bash", "fish": "terminal"
        default: "doc.text"
        }
    }
}

/// A command and what it printed, as a terminal shows them: long lines scroll sideways, and a
/// command that failed says how at the bottom.
struct TerminalCard: View {
    let command: String
    let output: String
    let failed: Bool
    @State private var all = false

    var body: some View {
        // The tool notes how a failed command ended after its output: that goes in a line of its own.
        let (printed, status) = Self.split(output)
        let lines = printed.components(separatedBy: "\n")
        let shown = all ? lines : Array(lines.prefix(14))
        VStack(alignment: .leading, spacing: 8) {
            ScrollView(.horizontal, showsIndicators: false) {
                VStack(alignment: .leading, spacing: 6) {
                    (Text("$ ").foregroundColor(Color.termPrompt) + Text(command).foregroundColor(Color.termInk))
                        .fontWeight(.semibold)
                    if !printed.isEmpty {
                        Text(shown.joined(separator: "\n")).foregroundColor(Color.termInk.opacity(0.72))
                    }
                }
                .font(.system(size: 11.5, design: .monospaced))
                .lineSpacing(2)
                .textSelection(.enabled)
                .fixedSize(horizontal: true, vertical: false)
            }
            if lines.count > shown.count {
                Button("Show all \(lines.count) lines") { withAnimation { all = true } }
                    .font(.caption.weight(.medium))
                    .foregroundStyle(Color.termPrompt)
            }
            if let status {
                Label(status, systemImage: "xmark.octagon.fill")
                    .font(.caption.weight(.medium))
                    .foregroundStyle(Color.termFail)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.termBack, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
    }

    /// The output, and the line saying how a failed command ended (`Command exited with code 2`).
    static func split(_ output: String) -> (String, String?) {
        let trimmed = output.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let range = trimmed.range(of: "Command exited with code ", options: .backwards),
              !trimmed[range.upperBound...].contains("\n") else { return (trimmed, nil) }
        let code = trimmed[range.upperBound...]
        return (String(trimmed[..<range.lowerBound]).trimmingCharacters(in: .whitespacesAndNewlines), "Exited with code \(code)")
    }
}

/// A tool's text output (a file, search results), quietly.
struct OutputCard: View {
    let text: String
    let failed: Bool
    @State private var all = false

    var body: some View {
        let lines = text.components(separatedBy: "\n")
        let shown = all ? lines : Array(lines.prefix(16))
        VStack(alignment: .leading, spacing: 6) {
            ScrollView(.horizontal, showsIndicators: false) {
                Text(shown.joined(separator: "\n"))
                    .font(.system(size: 12, design: .monospaced))
                    .foregroundStyle(failed ? AnyShapeStyle(.orange) : AnyShapeStyle(.secondary))
                    .textSelection(.enabled)
            }
            if lines.count > shown.count {
                Button("Show all \(lines.count) lines") { withAnimation { all = true } }
                    .font(.caption.weight(.medium))
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.codeCard, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(Color.hairline, lineWidth: 0.5))
    }
}

/// A change to a file: lines removed in red, added in green, with the lines around them.
struct DiffView: View {
    let diff: String

    struct Line: Hashable {
        var sign: Character
        var number: String
        var text: String
    }

    static func lines(_ diff: String) -> [Line] {
        diff.components(separatedBy: "\n").map { raw in
            guard let sign = raw.first else { return Line(sign: " ", number: "", text: "") }
            let rest = raw.dropFirst()
            let number = rest.prefix { $0 == " " || $0.isNumber }
            let digits = number.trimmingCharacters(in: .whitespaces)
            let text = rest.dropFirst(number.count)
            return Line(sign: sign, number: digits, text: digits.isEmpty && text.isEmpty ? String(rest).trimmingCharacters(in: .whitespaces) : String(text))
        }
    }

    static func counts(_ diff: String) -> (Int, Int) {
        let ls = lines(diff)
        return (ls.filter { $0.sign == "+" }.count, ls.filter { $0.sign == "-" }.count)
    }

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(Array(Self.lines(diff).enumerated()), id: \.offset) { _, l in
                    HStack(spacing: 8) {
                        Text(l.number).foregroundStyle(.tertiary).frame(minWidth: 28, alignment: .trailing)
                        Text(l.sign == " " ? " " : String(l.sign)).foregroundStyle(color(l.sign))
                        Text(l.text.isEmpty ? " " : l.text).foregroundStyle(l.sign == " " ? AnyShapeStyle(.secondary) : AnyShapeStyle(.primary))
                    }
                    .padding(.horizontal, 10)
                    .padding(.vertical, 1)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(wash(l.sign))
                }
            }
            .font(.system(size: 12, design: .monospaced))
            .padding(.vertical, 8)
        }
        .background(Color.codeCard, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(Color.hairline, lineWidth: 0.5))
        .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
    }

    private func color(_ sign: Character) -> Color { sign == "+" ? .green : sign == "-" ? .red : .clear }
    private func wash(_ sign: Character) -> Color { sign == "+" ? .green.opacity(0.12) : sign == "-" ? .red.opacity(0.12) : .clear }
}

extension Color {
    static let userBubble = Color.adaptive(light: Color(red: 0.93, green: 0.94, blue: 0.99), dark: Color(red: 0.19, green: 0.2, blue: 0.27))
    static let workCard = Color.adaptive(light: Color(white: 0.975), dark: Color(white: 0.1))
    static let termBack = Color.adaptive(light: Color(red: 0.11, green: 0.12, blue: 0.15), dark: Color(red: 0.06, green: 0.07, blue: 0.09))
    static let termInk = Color(white: 0.92)
    static let termPrompt = Color(red: 0.55, green: 0.85, blue: 0.6)
    static let termFail = Color(red: 1.0, green: 0.55, blue: 0.45)
}
