import SwiftUI

/// Markdown as an answer uses it: paragraphs, headings, lists, quotes and fenced code, with the
/// inline parts (emphasis, code, links) from the system's own reading of them.
struct MarkdownView: View {
    let text: String

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(MarkdownBlock.parse(text).enumerated()), id: \.offset) { _, block in
                switch block {
                case .paragraph(let t):
                    Text(inline(t)).textSelection(.enabled)
                case .heading(let level, let t):
                    Text(inline(t))
                        .font(level == 1 ? .title3.bold() : level == 2 ? .headline : .subheadline.bold())
                        .padding(.top, 4)
                case .item(let marker, let t, let depth):
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text(marker).foregroundStyle(.secondary).monospacedDigit()
                        Text(inline(t)).textSelection(.enabled)
                    }
                    .padding(.leading, CGFloat(depth) * 16)
                case .quote(let t):
                    Text(inline(t))
                        .foregroundStyle(.secondary)
                        .padding(.leading, 12)
                        .overlay(alignment: .leading) { Rectangle().fill(.tertiary).frame(width: 3) }
                case .code(let language, let code):
                    CodeBlock(language: language, code: code)
                case .rule:
                    Divider()
                }
            }
        }
    }

    private func inline(_ s: String) -> AttributedString {
        (try? AttributedString(markdown: s, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace))) ?? AttributedString(s)
    }
}

struct CodeBlock: View {
    let language: String
    let code: String
    @State private var copied = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text(language.isEmpty ? "code" : language)
                Spacer()
                Button {
                    UIPasteboard.general.string = code
                    copied = true
                    Task { try? await Task.sleep(for: .seconds(1.5)); copied = false }
                } label: {
                    Label(copied ? "Copied" : "Copy", systemImage: copied ? "checkmark" : "doc.on.doc")
                }
            }
            .font(.caption)
            .foregroundStyle(.secondary)
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            ScrollView(.horizontal, showsIndicators: false) {
                Text(code)
                    .font(.system(.footnote, design: .monospaced))
                    .textSelection(.enabled)
                    .padding(.horizontal, 12)
                    .padding(.bottom, 12)
            }
        }
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
    }
}

enum MarkdownBlock: Equatable {
    case paragraph(String)
    case heading(Int, String)
    /// A list item: its marker (`•`, `3.`), its text, and how deep it is.
    case item(String, String, Int)
    case quote(String)
    case code(String, String)
    case rule

    static func parse(_ text: String) -> [MarkdownBlock] {
        var out: [MarkdownBlock] = []
        var paragraph: [String] = []
        var lines = text.components(separatedBy: "\n")[...]
        func flush() {
            if !paragraph.isEmpty { out.append(.paragraph(paragraph.joined(separator: "\n"))) }
            paragraph = []
        }
        while let line = lines.popFirst() {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.hasPrefix("```") {
                flush()
                let language = String(trimmed.dropFirst(3)).trimmingCharacters(in: .whitespaces)
                var code: [String] = []
                while let l = lines.popFirst(), !l.trimmingCharacters(in: .whitespaces).hasPrefix("```") { code.append(l) }
                out.append(.code(language, code.joined(separator: "\n")))
            } else if trimmed.isEmpty {
                flush()
            } else if let h = heading(trimmed) {
                flush()
                out.append(h)
            } else if trimmed == "---" || trimmed == "***" || trimmed == "___" {
                flush()
                out.append(.rule)
            } else if let item = item(line) {
                flush()
                out.append(item)
            } else if trimmed.hasPrefix(">") {
                flush()
                out.append(.quote(String(trimmed.dropFirst()).trimmingCharacters(in: .whitespaces)))
            } else {
                paragraph.append(line)
            }
        }
        flush()
        return out
    }

    private static func heading(_ line: String) -> MarkdownBlock? {
        let hashes = line.prefix { $0 == "#" }.count
        guard (1...6).contains(hashes), line.dropFirst(hashes).first == " " else { return nil }
        return .heading(hashes, String(line.dropFirst(hashes + 1)))
    }

    private static func item(_ line: String) -> MarkdownBlock? {
        let indent = line.prefix { $0 == " " }.count
        let rest = line.dropFirst(indent)
        if let first = rest.first, "-*+".contains(first), rest.dropFirst().first == " " {
            return .item("•", String(rest.dropFirst(2)), indent / 2)
        }
        let digits = rest.prefix { $0.isNumber }
        if !digits.isEmpty, rest.dropFirst(digits.count).hasPrefix(". ") {
            return .item("\(digits).", String(rest.dropFirst(digits.count + 2)), indent / 2)
        }
        return nil
    }
}
