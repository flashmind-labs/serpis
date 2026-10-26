import SwiftUI

// How a chat's transcript reads: your messages in bubbles on the right, the agent's answers as
// text across the screen, what it did as one quiet line each (a tap shows the output), and
// notices small and centered.

/// One entry of a transcript.
struct EntryView: View {
    let entry: Entry

    var body: some View {
        switch entry {
        case .user(let text, let queued, let images):
            UserBubble(text: text, queued: queued, images: images)
        case .assistant(let blocks, let streaming, let error):
            AssistantView(blocks: blocks, streaming: streaming, error: error)
        case .tool(_, let name, let args, let partial, let result):
            ToolRow(name: name, args: args, partial: partial, result: result)
        case .info(let text, let tone):
            Text(text)
                .font(.footnote)
                .foregroundStyle(tone == "warning" || tone == "error" ? AnyShapeStyle(.orange) : AnyShapeStyle(.secondary))
                .multilineTextAlignment(.center)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 2)
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
                        .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
                }
            }
            if !text.isEmpty {
                Text(text)
                    .textSelection(.enabled)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)
                    .background(Color.accentColor.opacity(0.16), in: RoundedRectangle(cornerRadius: 20, style: .continuous))
            }
            if queued {
                Label("Sent while it works", systemImage: "clock")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: .infinity, alignment: .trailing)
        .padding(.leading, 48)
    }
}

struct AssistantView: View {
    let blocks: [Block]
    let streaming: Bool
    let error: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
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
            if streaming && !blocks.contains(where: { if case .text(let t) = $0 { !t.isEmpty } else { false } }) && !blocks.contains(where: { if case .thinking = $0 { true } else { false } }) {
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

/// The model's reasoning, folded away: a tap shows it.
struct Thought: View {
    let text: String
    let live: Bool
    @State private var open = false

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Button {
                withAnimation(.snappy) { open.toggle() }
            } label: {
                HStack(spacing: 4) {
                    Text(live ? "Thinking…" : "Thought")
                    Image(systemName: "chevron.right")
                        .font(.caption2.weight(.semibold))
                        .rotationEffect(.degrees(open ? 90 : 0))
                }
                .font(.subheadline)
                .foregroundStyle(.secondary)
            }
            .buttonStyle(.plain)
            if open {
                Text(text)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
                    .padding(.leading, 10)
                    .overlay(alignment: .leading) { Rectangle().fill(.quaternary).frame(width: 2) }
            }
        }
    }
}

/// The dots of a response that hasn't started saying anything.
struct Working: View {
    var body: some View {
        TimelineView(.animation) { context in
            let t = context.date.timeIntervalSinceReferenceDate
            HStack(spacing: 5) {
                ForEach(0..<3) { i in
                    Circle()
                        .frame(width: 7, height: 7)
                        .opacity(0.25 + 0.75 * max(0, sin(t * 4 - Double(i) * 0.7)))
                }
            }
            .foregroundStyle(.secondary)
        }
        .padding(.vertical, 6)
        .accessibilityLabel("Working")
    }
}

/// What a tool did, in a line: a tap shows its output.
struct ToolRow: View {
    let name: String
    let args: JSON
    let partial: String?
    let result: ToolResult?
    @State private var open = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Button {
                withAnimation(.snappy) { open.toggle() }
            } label: {
                HStack(spacing: 8) {
                    Image(systemName: icon)
                        .font(.footnote)
                        .frame(width: 18)
                    Text(verb).fontWeight(.medium) + Text(" ") + Text(summary).font(.system(.subheadline, design: .monospaced))
                    Spacer(minLength: 0)
                    status
                }
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            if open, let output = (result?.text ?? partial), !output.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    Text(clip(output))
                        .font(.system(.caption, design: .monospaced))
                        .foregroundStyle(result?.isError == true ? AnyShapeStyle(.orange) : AnyShapeStyle(.secondary))
                        .textSelection(.enabled)
                        .padding(10)
                }
                .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
            }
        }
    }

    @ViewBuilder private var status: some View {
        if result == nil {
            ProgressView().controlSize(.mini)
        } else if result?.isError == true {
            Image(systemName: "exclamationmark.circle").foregroundStyle(.orange)
        }
    }

    private var verb: String {
        switch name {
        case "bash": result == nil ? "Running" : "Ran"
        case "read": "Read"
        case "edit": "Edited"
        case "write": "Wrote"
        case "grep", "find": "Searched"
        case "ls": "Listed"
        case "web_search": "Searched the web"
        default: name
        }
    }

    private var icon: String {
        switch name {
        case "bash": "terminal"
        case "read": "doc.text"
        case "edit", "write": "pencil"
        case "grep", "find", "web_search": "magnifyingglass"
        case "ls": "folder"
        default: "wrench.and.screwdriver"
        }
    }

    /// The tool call in a few words, as the desktop says it.
    private var summary: String {
        let s = { (k: String) in args[k]?.string ?? "" }
        switch name {
        case "bash": return s("command").components(separatedBy: "\n").first ?? ""
        case "read", "edit", "write", "ls": return (s("path") as NSString).lastPathComponent
        case "grep": return s("pattern")
        case "find": return s("pattern")
        case "web_search": return s("query")
        default: return ""
        }
    }

    /// Long output, cut to what reads on a phone.
    private func clip(_ text: String) -> String {
        let lines = text.components(separatedBy: "\n")
        return lines.count > 60 ? (lines.prefix(60) + ["… \(lines.count - 60) more lines"]).joined(separator: "\n") : text
    }
}
