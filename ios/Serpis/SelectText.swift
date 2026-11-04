import SwiftUI
import UIKit

extension View {
    /// A long press offers to copy `text`, or to open it where any part of it can be selected.
    /// `markdown`: it is set as what it says, without its markup. `code`: in a fixed-width font.
    func selectable(_ text: String, markdown: Bool = false, code: Bool = false) -> some View {
        modifier(Selectable(text: text, markdown: markdown, code: code))
    }
}

private struct Selectable: ViewModifier {
    let text: String
    let markdown: Bool
    let code: Bool
    @State private var selecting = false

    func body(content: Content) -> some View {
        content
            .contextMenu {
                Button("Copy", systemImage: "doc.on.doc") { UIPasteboard.general.string = text }
                Button("Select Text", systemImage: "selection.pin.in.out") { selecting = true }
            } preview: {
                SelectablePreview(text: text, markdown: markdown, code: code)
            }
            .sheet(isPresented: $selecting) {
                SelectTextSheet(text: text, styled: markdown ? styledMarkdown(text) : NSAttributedString(string: text, attributes: code ? codeAttributes : bodyAttributes))
            }
    }
}

/// The start of it, as a long press lifts it: a long answer wouldn't fit the screen.
private struct SelectablePreview: View {
    let text: String
    let markdown: Bool
    let code: Bool

    var body: some View {
        let start = String(text.prefix(400))
        Text((markdown ? styledMarkdown(start).string : start) + (text.count > 400 ? "…" : ""))
            .font(code ? .system(.footnote, design: .monospaced) : .body)
            .padding()
            .frame(width: 320, alignment: .leading)
    }
}

private struct SelectTextSheet: View {
    let text: String
    let styled: NSAttributedString
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            SelectableTextView(text: styled)
                .ignoresSafeArea(edges: .bottom)
                .navigationTitle("Select Text")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Copy All") {
                            UIPasteboard.general.string = text
                            dismiss()
                        }
                    }
                    ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
                }
        }
    }
}

/// What SwiftUI's own text can't do on a phone: select part of it, with the handles and menu of
/// any text on iOS.
private struct SelectableTextView: UIViewRepresentable {
    let text: NSAttributedString

    func makeUIView(context: Context) -> UITextView {
        let view = UITextView()
        view.isEditable = false
        view.isSelectable = true
        view.textContainerInset = UIEdgeInsets(top: 12, left: 14, bottom: 32, right: 14)
        view.accessibilityIdentifier = "selectable-text"
        return view
    }

    func updateUIView(_ view: UITextView, context: Context) {
        if view.attributedText != text { view.attributedText = text }
    }
}

private var bodyAttributes: [NSAttributedString.Key: Any] {
    [.font: UIFont.preferredFont(forTextStyle: .body), .foregroundColor: UIColor.label]
}

private var codeAttributes: [NSAttributedString.Key: Any] {
    [.font: UIFont.monospacedSystemFont(ofSize: UIFont.preferredFont(forTextStyle: .footnote).pointSize, weight: .regular), .foregroundColor: UIColor.label]
}

/// Markdown as text to select: its emphasis, code and links set, and its markup gone where that
/// would get in the way of what's copied. Code blocks stay exactly as written.
func styledMarkdown(_ markdown: String) -> NSAttributedString {
    let out = NSMutableAttributedString()
    var lines: [String] = []
    var inCode = false
    func flush() {
        guard !lines.isEmpty else { return }
        let part = lines.joined(separator: "\n")
        lines = []
        if !out.string.isEmpty { out.append(NSAttributedString(string: "\n\n", attributes: bodyAttributes)) }
        out.append(inCode ? NSAttributedString(string: part, attributes: codeAttributes) : styledProse(part))
    }
    for line in markdown.components(separatedBy: "\n") {
        if line.trimmingCharacters(in: .whitespaces).hasPrefix("```") {
            flush()
            inCode.toggle()
        } else {
            lines.append(line)
        }
    }
    flush()
    return out
}

private func styledProse(_ prose: String) -> NSAttributedString {
    let trimmed = prose.trimmingCharacters(in: .newlines)
    // Headings read as bold lines.
    let source = trimmed.replacingOccurrences(of: #"(?m)^#{1,6}[ \t]+(.+)$"#, with: "**$1**", options: .regularExpression)
    guard let parsed = try? AttributedString(markdown: source, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)) else {
        return NSAttributedString(string: trimmed, attributes: bodyAttributes)
    }
    let body = UIFont.preferredFont(forTextStyle: .body)
    let out = NSMutableAttributedString()
    for run in parsed.runs {
        var traits: UIFontDescriptor.SymbolicTraits = []
        if run.inlinePresentationIntent?.contains(.stronglyEmphasized) == true { traits.insert(.traitBold) }
        if run.inlinePresentationIntent?.contains(.emphasized) == true { traits.insert(.traitItalic) }
        var attributes = bodyAttributes
        if run.inlinePresentationIntent?.contains(.code) == true {
            attributes[.font] = UIFont.monospacedSystemFont(ofSize: body.pointSize * 0.9, weight: .regular)
        } else if let d = body.fontDescriptor.withSymbolicTraits(traits) {
            attributes[.font] = UIFont(descriptor: d, size: 0)
        }
        if let link = run.link { attributes[.link] = link }
        out.append(NSAttributedString(string: String(parsed[run.range].characters), attributes: attributes))
    }
    return out
}
