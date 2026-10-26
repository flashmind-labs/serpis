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
}
