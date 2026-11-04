import UIKit
import XCTest

/// A long chat, written as a session on the test machine, opened and read as a person would:
/// how long it takes to show, and how many frames come late scrolling it and opening what it
/// folded. The numbers go to the log as `PERF …`.
final class LongChatTests: XCTestCase {
    private var env: [String: String] { ProcessInfo.processInfo.environment }

    /// A session of `turns` turns, each a question, thoughts, tools with their output, and a
    /// Markdown answer. Nothing real: made up here.
    static func session(turns: Int, cwd: String) -> String {
        func line(_ o: Any) -> String { String(data: try! JSONSerialization.data(withJSONObject: o), encoding: .utf8)! }
        var parent: Any = NSNull()
        var n = 0
        var out = [line(["type": "session", "version": 3, "id": "long-chat", "timestamp": "2026-01-01T00:00:00.000Z", "cwd": cwd])]
        func message(_ m: [String: Any]) {
            n += 1
            out.append(line(["type": "message", "id": "e\(n)", "parentId": parent, "timestamp": "2026-01-01T00:00:00.000Z", "message": m]))
            parent = "e\(n)"
        }
        let prose = String(repeating: "The build **links** std statically, so `the binary` runs anywhere; see [the docs](https://example.com) for what that costs on each platform. ", count: 5)
        let answer = "## Part NUM\n\n\(prose)\n\n\(prose)\n\n1. **Fetch** the crates\n2. Build with `cargo build --release`\n   - the unit tests, in about *ten seconds*\n   - the UI suite\n\n| Step | Cold | Warm |\n|---|---:|---:|\n| Fetch | 40s | 0s |\n| Build | 3m 10s | 12s |\n\n```sh\ncargo build --release\ncargo test --workspace\n```\n\n> Release builds run anywhere.\n\n\(prose)"
        let printed = (0..<300).map { "src/module\($0).rs:\($0): a line of what the command printed" }.joined(separator: "\n")
        // A photo's worth of picture, as a phone sends them.
        let side = CGSize(width: 2048, height: 1536)
        let picture = UIGraphicsImageRenderer(size: side, format: { let f = UIGraphicsImageRendererFormat(); f.scale = 1; return f }()).image { c in
            for i in 0..<400 {
                UIColor(hue: CGFloat(i) / 400, saturation: 0.7, brightness: 0.8, alpha: 1).setFill()
                c.fill(CGRect(x: CGFloat(i * 37 % 2048), y: CGFloat(i * 91 % 1536), width: 160, height: 160))
            }
        }.jpegData(compressionQuality: 0.85)!.base64EncodedString()
        let usage: [String: Any] = ["input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0, "cost": ["input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0]]
        for t in 0..<turns {
            let question: [[String: Any]] = [["type": "text", "text": "Question \(t): how does the build work?"]] + (t % 10 == 0 ? [["type": "image", "data": picture, "mimeType": "image/jpeg"]] : [])
            message(["role": "user", "content": question, "timestamp": t])
            message(["role": "assistant", "content": [
                ["type": "thinking", "thinking": String(repeating: "Thinking about what the build does and what to look at first. ", count: 30)],
                ["type": "toolCall", "id": "c\(t)a", "name": "bash", "arguments": ["command": "grep -rn build src"]],
                ["type": "toolCall", "id": "c\(t)b", "name": "read", "arguments": ["path": "/x/src/main.rs"]],
            ], "api": "openai-completions", "provider": "mock", "model": "m", "usage": usage, "stopReason": "toolUse", "timestamp": t])
            message(["role": "toolResult", "toolCallId": "c\(t)a", "toolName": "bash", "content": [["type": "text", "text": printed]], "isError": false, "timestamp": t])
            message(["role": "toolResult", "toolCallId": "c\(t)b", "toolName": "read", "content": [["type": "text", "text": printed]], "isError": false, "timestamp": t])
            message(["role": "assistant", "content": [
                ["type": "thinking", "thinking": String(repeating: "Now I can say how it works, in parts. ", count: 30)],
                ["type": "text", "text": answer.replacingOccurrences(of: "NUM", with: "\(t)")],
            ], "api": "openai-completions", "provider": "mock", "model": "m", "usage": usage, "stopReason": "stop", "timestamp": t])
        }
        return out.joined(separator: "\n") + "\n"
    }

    /// Late frames since `from`: how many, in total, and the worst, in milliseconds.
    private func late(_ log: String, since from: Double) -> (count: Int, total: Int, worst: Int) {
        let rows = ((try? String(contentsOfFile: log, encoding: .utf8)) ?? "").split(separator: "\n").compactMap { row -> Double? in
            let p = row.split(separator: " ")
            guard p.count == 2, let at = Double(p[0]), at >= from else { return nil }
            return Double(p[1])
        }
        return (rows.count, Int(rows.reduce(0, +)), Int(rows.max() ?? 0))
    }

    private func shot(_ name: String) {
        guard let dir = env["SERPIS_SHOTS"] else { return }
        try? XCUIScreen.main.screenshot().pngRepresentation.write(to: URL(fileURLWithPath: dir).appendingPathComponent("long-\(name).png"))
    }

    func testALongChat() throws {
        guard let address = env["SERPIS_TEST_ADDRESS"], let key = env["SERPIS_TEST_KEY"], let home = env["SERPIS_TEST_HOME"] else {
            throw XCTSkip("no test machine: run ios/test-machine.sh and pass its environment as TEST_RUNNER_*")
        }
        let cwd = home + "/project"
        let root = URL(fileURLWithPath: home).deletingLastPathComponent().appendingPathComponent("sessions/--\(cwd.dropFirst().replacingOccurrences(of: "/", with: "-"))--")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        try Self.session(turns: 300, cwd: cwd).write(to: root.appendingPathComponent("2026-01-01T00-00-00-000Z_long-chat.jsonl"), atomically: true, encoding: .utf8)

        let log = NSTemporaryDirectory() + "serpis-frames-\(UUID().uuidString)"
        let app = XCUIApplication()
        app.launchArguments = ["-SerpisMachines", #"[{"id":"box","name":"box","address":"\#(address)"}]"#, "-SerpisDeviceKey", key, "-SerpisData", NSTemporaryDirectory() + "serpis-ui-\(UUID().uuidString)", "-SerpisFrameLog", log]
        app.launch()
        let confirm = app.buttons["Confirm box's key to reach it"]
        XCTAssertTrue(confirm.waitForExistence(timeout: 20))
        confirm.tap()
        let trust = app.buttons["Trust it"]
        XCTAssertTrue(trust.waitForExistence(timeout: 5))
        trust.tap()
        XCTAssertTrue(trust.waitForNonExistence(timeout: 20))
        app.buttons["Done"].tap()
        // (Started again, it lists what the machine has.)
        app.terminate()
        app.launch()

        let row = app.buttons.matching(identifier: "chat-row").firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: 30))
        var from = Date().timeIntervalSince1970
        row.tap()
        let said = app.descendants(matching: .any).matching(NSPredicate(format: "label CONTAINS 'runs anywhere'")).firstMatch
        XCTAssertTrue(said.waitForExistence(timeout: 60), "the chat shows")
        shot("open")
        print("PERF open seconds=\(String(format: "%.2f", Date().timeIntervalSince1970 - from)) late=\(late(log, since: from))")

        let transcript = app.scrollViews["transcript"]
        Thread.sleep(forTimeInterval: 1)
        from = Date().timeIntervalSince1970
        FileManager.default.createFile(atPath: log + ".scroll", contents: nil)
        while !FileManager.default.fileExists(atPath: log + ".done"), Date().timeIntervalSince1970 - from < 60 {
            Thread.sleep(forTimeInterval: 0.2)
        }
        print("PERF scroll seconds=\(String(format: "%.2f", Date().timeIntervalSince1970 - from)) late=\(late(log, since: from))")
        shot("scrolled")
        // Folded work on screen, to open and close.
        var w: XCUIElement?
        for _ in 0..<20 {
            w = app.buttons.matching(identifier: "work").allElementsBoundByIndex.first { $0.isHittable && $0.frame.minY > 150 && $0.frame.maxY < 500 }
            if w != nil { break }
            transcript.swipeDown(velocity: .slow)
        }
        let fold = try XCTUnwrap(w, "a stretch of work on screen")
        from = Date().timeIntervalSince1970
        // (By its place on the screen: finding it again is XCUITest's own cost, not the app's.)
        let screen = app.windows.firstMatch.frame.size
        let at = app.coordinate(withNormalizedOffset: CGVector(dx: fold.frame.midX / screen.width, dy: fold.frame.midY / screen.height))
        // A control: taps that open nothing cost XCUITest the same, so what's measured is the difference.
        let nothing = app.navigationBars.firstMatch.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.1))
        from = Date().timeIntervalSince1970
        for _ in 0..<6 { nothing.tap() }
        print("PERF control seconds=\(String(format: "%.2f", Date().timeIntervalSince1970 - from)) late=\(late(log, since: from))")
        Thread.sleep(forTimeInterval: 0.5)
        from = Date().timeIntervalSince1970
        for _ in 0..<6 { at.tap() }
        shot("toggled")
        print("PERF work seconds=\(String(format: "%.2f", Date().timeIntervalSince1970 - from)) late=\(late(log, since: from))")
    }
}
