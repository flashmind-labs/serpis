import MarkdownUI
import SwiftUI

/// An answer's Markdown, the way a chat app sets it: body text with room to breathe, headings
/// that step down clearly, lists with real markers and indents, tables that scroll sideways when
/// they're wide, and code in cards with their language and a copy button.
struct MarkdownView: View {
    let text: String

    var body: some View {
        Markdown(text)
            .markdownTheme(.serpis)
            .textSelection(.enabled)
    }
}

extension Theme {
    static let serpis = Theme()
        .text {
            FontSize(17)
        }
        .code {
            FontFamilyVariant(.monospaced)
            FontSize(.em(0.84))
            ForegroundColor(Color.codeInk)
            BackgroundColor(Color.codeWash)
        }
        .strong { FontWeight(.semibold) }
        .emphasis { FontStyle(.italic) }
        .link { ForegroundColor(.accentColor) }
        .heading1 { c in
            c.label
                .relativeLineSpacing(.em(0.1))
                .markdownMargin(top: 22, bottom: 10)
                .markdownTextStyle { FontWeight(.bold); FontSize(.em(1.4)) }
        }
        .heading2 { c in
            c.label
                .relativeLineSpacing(.em(0.1))
                .markdownMargin(top: 20, bottom: 8)
                .markdownTextStyle { FontWeight(.bold); FontSize(.em(1.2)) }
        }
        .heading3 { c in
            c.label
                .markdownMargin(top: 18, bottom: 6)
                .markdownTextStyle { FontWeight(.semibold); FontSize(.em(1.06)) }
        }
        .heading4 { c in
            c.label
                .markdownMargin(top: 16, bottom: 4)
                .markdownTextStyle { FontWeight(.semibold) }
        }
        .heading5 { c in
            c.label
                .markdownMargin(top: 14, bottom: 4)
                .markdownTextStyle { FontWeight(.semibold); FontSize(.em(0.92)) }
        }
        .heading6 { c in
            c.label
                .markdownMargin(top: 14, bottom: 4)
                .markdownTextStyle { FontWeight(.semibold); FontSize(.em(0.88)); ForegroundColor(.secondary) }
        }
        .paragraph { c in
            c.label
                .fixedSize(horizontal: false, vertical: true)
                .relativeLineSpacing(.em(0.28))
                .markdownMargin(top: 0, bottom: 12)
        }
        .list { c in
            c.label.markdownMargin(top: 0, bottom: 12)
        }
        .listItem { c in
            c.label.markdownMargin(top: .em(0.35))
        }
        .bulletedListMarker { _ in
            Circle()
                .fill(.secondary)
                .frame(width: 5, height: 5)
                .relativeFrame(minWidth: .em(1.3), alignment: .center)
        }
        .numberedListMarker { c in
            Text("\(c.itemNumber).")
                .monospacedDigit()
                .foregroundStyle(.secondary)
                .relativeFrame(minWidth: .em(1.5), alignment: .trailing)
        }
        .taskListMarker { c in
            Image(systemName: c.isCompleted ? "checkmark.circle.fill" : "circle")
                .foregroundStyle(c.isCompleted ? AnyShapeStyle(Color.accentColor) : AnyShapeStyle(.tertiary))
                .relativeFrame(minWidth: .em(1.5), alignment: .center)
        }
        .blockquote { c in
            HStack(spacing: 0) {
                RoundedRectangle(cornerRadius: 2)
                    .fill(Color.accentColor.opacity(0.5))
                    .frame(width: 3)
                c.label
                    .markdownTextStyle { ForegroundColor(.secondary) }
                    .padding(.leading, 12)
            }
            .fixedSize(horizontal: false, vertical: true)
            .markdownMargin(top: 0, bottom: 12)
        }
        .codeBlock { c in
            CodeCard(language: c.language ?? "", code: c.content) {
                c.label
                    .relativeLineSpacing(.em(0.3))
                    .markdownTextStyle {
                        FontFamilyVariant(.monospaced)
                        FontSize(.em(0.8))
                    }
            }
            .markdownMargin(top: 2, bottom: 14)
        }
        .table { c in
            ScrollView(.horizontal, showsIndicators: false) {
                c.label
                    .fixedSize(horizontal: false, vertical: true)
                    .markdownTableBorderStyle(.init(.horizontalBorders, color: Color.hairline))
            }
            .markdownMargin(top: 2, bottom: 14)
        }
        .tableCell { c in
            c.label
                .markdownTextStyle {
                    if c.row == 0 { FontWeight(.semibold) }
                    FontSize(.em(0.9))
                }
                .fixedSize(horizontal: false, vertical: true)
                .padding(.vertical, 8)
                .padding(.horizontal, 10)
        }
        .thematicBreak {
            Rectangle()
                .fill(Color.hairline)
                .frame(height: 1)
                .markdownMargin(top: 16, bottom: 16)
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
