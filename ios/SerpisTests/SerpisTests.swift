@testable import Serpis
import XCTest

final class SerpisTests: XCTestCase {
    func testMarkdownReadsBlocks() {
        let text = "# Hi\n\n- a\n  - b\n1. c\n```sh\nls\n```\ntext\nmore"
        XCTAssertEqual(MarkdownBlock.parse(text), [
            .heading(1, "Hi"), .item("•", "a", 0), .item("•", "b", 1), .item("1.", "c", 0), .code("sh", "ls"), .paragraph("text\nmore"),
        ])
    }

    func testSplicesReplaceTheTail() throws {
        var t: [Entry] = []
        let first = try JSONDecoder().decode(Splice.self, from: Data(#"{"from":0,"entries":[{"kind":"user","text":"hi","queued":false},{"kind":"info","text":"x","tone":"meta"}]}"#.utf8))
        first.apply(to: &t)
        let second = try JSONDecoder().decode(Splice.self, from: Data(#"{"from":1,"entries":[{"kind":"assistant","blocks":[{"type":"text","text":"ok"}],"model":"m","streaming":false,"continuation":false}]}"#.utf8))
        second.apply(to: &t)
        XCTAssertEqual(t, [.user(text: "hi", queued: false, images: []), .assistant(blocks: [.text("ok")], streaming: false, error: nil)])
    }
}
