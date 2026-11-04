@testable import Serpis
import XCTest

final class SerpisTests: XCTestCase {
    func testSplicesReplaceTheTail() throws {
        var t: [Entry] = []
        let first = try JSONDecoder().decode(Splice.self, from: Data(#"{"from":0,"entries":[{"kind":"user","text":"hi","queued":false},{"kind":"info","text":"x","tone":"meta"}]}"#.utf8))
        first.apply(to: &t)
        let second = try JSONDecoder().decode(Splice.self, from: Data(#"{"from":1,"entries":[{"kind":"assistant","blocks":[{"type":"text","text":"ok"}],"model":"m","streaming":false,"continuation":false}]}"#.utf8))
        second.apply(to: &t)
        XCTAssertEqual(t, [.user(text: "hi", queued: false, images: []), .assistant(blocks: [.text("ok")], streaming: false, error: nil)])
    }

    func testChatsAreGroupedByWhen() {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        func row(_ id: String, ago: Double, running: Bool = false) -> ChatRow {
            ChatRow(machine: "m", chat: id, title: id, cwd: "", modified: now.addingTimeInterval(-ago), running: running)
        }
        let rows = [row("a", ago: 60, running: true), row("b", ago: 60), row("c", ago: 86_400 * 1.2), row("d", ago: 86_400 * 4), row("e", ago: 86_400 * 30)]
        XCTAssertEqual(ChatsView.sections(rows, now: now).map(\.title), ["Working", "Today", "Yesterday", "Previous 7 days", "Older"])
        XCTAssertEqual(ChatsView.sections(rows, now: now).map { $0.rows.map(\.chat) }, [["a"], ["b"], ["c"], ["d"], ["e"]])
    }

    @MainActor func testAChatIsLoadingUntilItsTranscriptComes() throws {
        let chat = ChatModel(app: AppModel(store: UserDefaults(suiteName: "serpis-unit-tests")!), machine: "m", chat: "c", path: nil)
        XCTAssertTrue(chat.loading)
        chat.take(.splice(chat: "c", Splice(from: 0, entries: [])))
        XCTAssertFalse(chat.loading)
    }

    func testThinkingLevelsAModelOffers() throws {
        func levels(_ json: String) throws -> [String] {
            try JSONDecoder().decode(ModelInfo.self, from: Data(json.utf8)).thinkingLevels
        }
        XCTAssertEqual(try levels(#"{"model":"a","name":"A","images":false,"reasoning":true,"thinking_levels":["off","high","max"]}"#), ["off", "high", "max"])
        XCTAssertEqual(try levels(#"{"model":"a","name":"A","images":false,"reasoning":false,"thinking_levels":["off"]}"#), [], "nothing to choose")
        XCTAssertEqual(try levels(#"{"model":"a","name":"A","images":false,"reasoning":true}"#), ["off", "low", "medium", "high"], "a node from before the field")
        XCTAssertEqual(try levels(#"{"model":"a","name":"A","images":false,"reasoning":false}"#), [])
        XCTAssertEqual(thinkingName("xhigh"), "Extra high")
    }

    func testMarkdownToSelectLosesItsMarkupButNotItsCode() {
        let text = styledMarkdown("## Steps\n\nA **bold** and `code` word.\n\n```sh\necho **raw** *\n```\n\nThe [end](https://example.com).").string
        XCTAssertEqual(text, "Steps\n\nA bold and code word.\n\necho **raw** *\n\nThe end.")
    }

    func testMarkdownBlocks() {
        let blocks = MarkdownBlock.parse("# Title\n\nSome **text**.\n\n1. One\n   - [x] done\n\n> said\n\n---\n\n| A | B |\n|---|--:|\n| 1 |  |\n\n```sh\nls\n```")
        func described(_ b: MarkdownBlock) -> String {
            switch b.kind {
            case .heading(let l, let t): "h\(l) \(String(t.characters))"
            case .text(let t, let depth, let marker, let quoted): "p\(depth) \(marker.map { "\($0)" } ?? "-") \(quoted) \(String(t.characters))"
            case .code(let language, let code): "code \(language) \(code)"
            case .table(let columns, let rows): "table \(columns.count) \(rows.map { $0.map { String($0.characters) } })"
            case .rule: "rule"
            }
        }
        XCTAssertEqual(blocks.map(described), [
            "h1 Title",
            "p0 - false Some text.",
            #"p1 number("1.") false One"#,
            "p2 task(done: true) false done",
            "p0 - true said",
            "rule",
            #"table 2 [["A", "B"], ["1", ""]]"#,
            "code sh ls",
        ])
    }
}
