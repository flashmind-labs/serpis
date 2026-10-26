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
}
