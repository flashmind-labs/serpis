import SwiftUI

/// An answer's Markdown, the way a chat app sets it: body text with room to breathe, headings
/// that step down clearly, lists with real markers and indents, tables that scroll sideways when
/// they're wide, and code in cards with their language and a copy button. Each block is one
/// `Text`, parsed once: long chats have to scroll without a hitch.
struct MarkdownView: View {
    let text: String

    var body: some View {
        let blocks = MarkdownBlock.cached(text)
        VStack(alignment: .leading, spacing: 0) {
            ForEach(blocks) { block in
                MarkdownBlockView(block: block)
                    .padding(.top, block.id == 0 ? 0 : block.gap)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .selectable(text, markdown: true)
    }
}

/// A block of Markdown, as it is shown.
struct MarkdownBlock: Identifiable {
    enum Marker: Equatable { case bullet, number(String), task(done: Bool) }
    enum Kind {
        case heading(Int, AttributedString)
        /// A paragraph; in a list, how deep and (its first) with what marker; maybe quoted.
        case text(AttributedString, depth: Int, marker: Marker?, quoted: Bool)
        case code(language: String, String)
        case table(columns: [PresentationIntent.TableColumn], rows: [[AttributedString]])
        case rule
    }

    var id: Int
    var kind: Kind
    /// The room above it.
    var gap: CGFloat

    private static let cache = NSCache<NSString, Parsed>()
    private final class Parsed {
        let blocks: [MarkdownBlock]
        init(_ blocks: [MarkdownBlock]) { self.blocks = blocks }
    }

    static func cached(_ markdown: String) -> [MarkdownBlock] {
        let key = markdown as NSString
        if let hit = cache.object(forKey: key) { return hit.blocks }
        let blocks = parse(markdown)
        cache.setObject(Parsed(blocks), forKey: key)
        return blocks
    }

    static func parse(_ markdown: String) -> [MarkdownBlock] {
        guard var doc = try? AttributedString(markdown: markdown, options: .init(interpretedSyntax: .full, failurePolicy: .returnPartiallyParsedIfPossible)) else {
            return [MarkdownBlock(id: 0, kind: .text(AttributedString(markdown), depth: 0, marker: nil, quoted: false), gap: 0)]
        }
        let code = doc.runs.filter { $0.inlinePresentationIntent?.contains(.code) == true }.map(\.range)
        for range in code {
            doc[range].foregroundColor = Color.codeInk
            doc[range].backgroundColor = Color.codeWash
        }

        var out: [MarkdownBlock] = []
        var items = Set<Int>()
        var table: (id: Int, row: Int)?
        func add(_ kind: Kind, gap: CGFloat) { out.append(MarkdownBlock(id: out.count, kind: kind, gap: gap)) }

        for (intent, range) in doc.runs[\.presentationIntent] {
            let parts = intent?.components ?? []
            var piece = AttributedString(doc[range])
            piece.presentationIntent = nil
            switch parts.first?.kind {
            case .header(let level):
                add(.heading(level, piece), gap: [22, 20, 18, 16, 14, 14][min(level, 6) - 1])
            case .codeBlock(let language):
                var text = String(piece.characters)
                if text.hasSuffix("\n") { text.removeLast() }
                add(.code(language: language ?? "", text), gap: 14)
            case .thematicBreak:
                add(.rule, gap: 16)
            case .tableCell(let column):
                guard let row = parts.dropFirst().first, let whole = parts.first(where: { if case .table = $0.kind { true } else { false } }),
                      case .table(let columns) = whole.kind else { continue }
                if table?.id != whole.identity {
                    add(.table(columns: columns, rows: []), gap: 14)
                    table = (whole.identity, -1)
                }
                guard case .table(let cs, var rows) = out[out.count - 1].kind else { continue }
                if table?.row != row.identity {
                    rows.append(Array(repeating: AttributedString(), count: cs.count))
                    table?.row = row.identity
                }
                if rows[rows.count - 1].indices.contains(column) { rows[rows.count - 1][column] = piece }
                out[out.count - 1].kind = .table(columns: cs, rows: rows)
            default:
                let lists = parts.filter { if case .listItem = $0.kind { true } else { false } }
                let quoted = parts.contains { $0.kind == .blockQuote }
                var marker: Marker?
                if let item = lists.first, items.insert(item.identity).inserted, case .listItem(let ordinal) = item.kind {
                    let list = parts.first { p in
                        switch p.kind {
                        case .orderedList, .unorderedList: true
                        default: false
                        }
                    }
                    marker = list?.kind == .orderedList ? .number("\(ordinal).") : .bullet
                    let start = String(piece.characters.prefix(4))
                    if start == "[ ] " || start.lowercased() == "[x] " {
                        marker = .task(done: start != "[ ] ")
                        piece.removeSubrange(piece.startIndex..<piece.index(piece.startIndex, offsetByCharacters: 4))
                    }
                }
                // Items of a list sit close; what follows it, a paragraph away.
                let inList: Bool = if case .text(_, let depth, _, _)? = out.last?.kind { depth > 0 && !lists.isEmpty } else { false }
                add(.text(piece, depth: lists.count, marker: marker, quoted: quoted), gap: inList ? 6 : 12)
            }
        }
        return out
    }
}

private struct MarkdownBlockView: View {
    let block: MarkdownBlock
    private static let size: CGFloat = 17
    private static let markerWidth: CGFloat = 24

    var body: some View {
        switch block.kind {
        case .heading(let level, let text):
            Text(text)
                .font(.system(size: Self.size * [1.4, 1.2, 1.06, 1, 0.92, 0.88][min(level, 6) - 1], weight: level <= 2 ? .bold : .semibold))
                .foregroundStyle(level == 6 ? AnyShapeStyle(.secondary) : AnyShapeStyle(.primary))
                .accessibilityAddTraits(.isHeader)
        case .text(let text, let depth, let marker, let quoted):
            HStack(alignment: .firstTextBaseline, spacing: 0) {
                if depth > 0 {
                    markerView(marker).frame(width: Self.markerWidth, alignment: .leading)
                }
                Text(text)
                    .font(.system(size: Self.size))
                    .lineSpacing(5)
                    .foregroundStyle(quoted ? AnyShapeStyle(.secondary) : AnyShapeStyle(.primary))
            }
            .padding(.leading, CGFloat(max(depth - 1, 0)) * Self.markerWidth + (quoted ? 15 : 0))
            .overlay(alignment: .leading) {
                if quoted {
                    RoundedRectangle(cornerRadius: 2).fill(Color.accentColor.opacity(0.5)).frame(width: 3)
                }
            }
        case .code(let language, let code):
            CodeCard(language: language, code: code) {
                Text(code)
                    .font(.system(size: 13.5, design: .monospaced))
                    .lineSpacing(4)
                    .fixedSize(horizontal: true, vertical: true)
            }
        case .table(let columns, let rows):
            ScrollView(.horizontal, showsIndicators: false) {
                Grid(alignment: .leading, horizontalSpacing: 0, verticalSpacing: 0) {
                    ForEach(rows.indices, id: \.self) { r in
                        if r > 0 { Rectangle().fill(Color.hairline).frame(height: 0.5) }
                        GridRow {
                            ForEach(rows[r].indices, id: \.self) { c in
                                Text(rows[r][c])
                                    .font(.system(size: 15, weight: r == 0 ? .semibold : .regular))
                                    .fixedSize(horizontal: false, vertical: true)
                                    .padding(.vertical, 8)
                                    .padding(.horizontal, 10)
                                    .gridColumnAlignment(columns[c].alignment == .right ? .trailing : columns[c].alignment == .center ? .center : .leading)
                            }
                        }
                    }
                }
            }
        case .rule:
            Rectangle().fill(Color.hairline).frame(height: 1)
        }
    }

    @ViewBuilder private func markerView(_ marker: MarkdownBlock.Marker?) -> some View {
        switch marker {
        case .bullet: Text("•").font(.system(size: Self.size)).foregroundStyle(.secondary)
        case .number(let n): Text(n).font(.system(size: Self.size).monospacedDigit()).foregroundStyle(.secondary)
        case .task(let done):
            Image(systemName: done ? "checkmark.circle.fill" : "circle")
                .font(.system(size: Self.size - 2))
                .foregroundStyle(done ? AnyShapeStyle(Color.accentColor) : AnyShapeStyle(.tertiary))
        case nil: Color.clear.frame(height: 1)
        }
    }
}

/// Code in a card: its language and a copy button on top, the code scrolling sideways under it.
struct CodeCard<Code: View>: View {
    let language: String
    let code: String
    @ViewBuilder let content: () -> Code
    @State private var copied = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text(language.isEmpty ? "code" : language)
                    .font(.caption.weight(.medium))
                    .foregroundStyle(.secondary)
                Spacer()
                Button {
                    UIPasteboard.general.string = code
                    withAnimation { copied = true }
                    UIImpactFeedbackGenerator(style: .light).impactOccurred()
                    Task {
                        try? await Task.sleep(for: .seconds(1.5))
                        withAnimation { copied = false }
                    }
                } label: {
                    Image(systemName: copied ? "checkmark" : "square.on.square")
                        .font(.caption.weight(.medium))
                        .foregroundStyle(copied ? AnyShapeStyle(Color.accentColor) : AnyShapeStyle(.secondary))
                        .contentTransition(.symbolEffect(.replace))
                        .frame(width: 28, height: 22)
                }
                .accessibilityLabel(copied ? "Copied" : "Copy code")
            }
            .padding(.leading, 12)
            .padding(.trailing, 6)
            .padding(.vertical, 4)
            .background(Color.codeWash.opacity(0.6))
            ScrollView(.horizontal, showsIndicators: false) {
                content()
                    .padding(.horizontal, 12)
                    .padding(.vertical, 10)
            }
        }
        .background(Color.codeCard, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(Color.hairline, lineWidth: 0.5))
    }
}

extension Color {
    /// The wash behind inline code and table stripes.
    static let codeWash = Color.adaptive(light: Color(white: 0.94), dark: Color(white: 0.17))
    static let codeInk = Color.adaptive(light: Color(red: 0.62, green: 0.15, blue: 0.36), dark: Color(red: 1.0, green: 0.62, blue: 0.78))
    static let codeCard = Color.adaptive(light: Color(white: 0.975), dark: Color(white: 0.11))
    static let hairline = Color.adaptive(light: Color(white: 0.86), dark: Color(white: 0.24))

    static func adaptive(light: Color, dark: Color) -> Color {
        Color(UIColor { $0.userInterfaceStyle == .dark ? UIColor(dark) : UIColor(light) })
    }
}
