import UIKit

/// Under UI tests (`-SerpisFrameLog <file>`), the frames that came late, one per line: when, in
/// seconds since 1970, and how late in milliseconds. It's how the tests tell a chat that scrolls
/// smoothly from one that stutters.
///
/// The test asks for a scroll by creating `<file>.scroll`; the probe scrolls the transcript up by
/// itself and writes `<file>.done` at the end. (XCUITest's own gestures take a snapshot of the
/// screen's accessibility first, on the main thread, which is most of what they'd measure.)
@MainActor
final class FrameProbe: NSObject {
    private static var running: FrameProbe?
    private let out: FileHandle
    private var last: CFTimeInterval = 0
    private let path: String
    private var scrolling: UIScrollView?
    private var started: CFTimeInterval = 0
    private static let speed: CGFloat = 3000
    private static let seconds: CFTimeInterval = 8

    static func startIfAsked() {
        let args = ProcessInfo.processInfo.arguments
        guard running == nil, let i = args.firstIndex(of: "-SerpisFrameLog"), args.indices.contains(i + 1) else { return }
        FileManager.default.createFile(atPath: args[i + 1], contents: nil)
        guard let out = FileHandle(forWritingAtPath: args[i + 1]) else { return }
        let probe = FrameProbe(out, path: args[i + 1])
        running = probe
        CADisplayLink(target: probe, selector: #selector(tick)).add(to: .main, forMode: .common)
    }

    private init(_ out: FileHandle, path: String) { (self.out, self.path) = (out, path) }

    /// The tallest scroll view on screen: the transcript.
    private static func transcript(in view: UIView) -> UIScrollView? {
        var best = view as? UIScrollView
        for sub in view.subviews {
            if let found = transcript(in: sub), found.contentSize.height > (best?.contentSize.height ?? 0) { best = found }
        }
        return best
    }

    private func scroll(_ link: CADisplayLink) {
        if scrolling == nil {
            guard FileManager.default.fileExists(atPath: path + ".scroll") else { return }
            try? FileManager.default.removeItem(atPath: path + ".scroll")
            let windows = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.flatMap(\.windows)
            scrolling = windows.lazy.compactMap { Self.transcript(in: $0) }.first
            started = link.timestamp
        }
        guard let view = scrolling else { return }
        view.contentOffset.y = max(0, view.contentOffset.y - Self.speed * link.duration)
        if link.timestamp - started > Self.seconds {
            scrolling = nil
            FileManager.default.createFile(atPath: path + ".done", contents: nil)
        }
    }

    @objc private func tick(_ link: CADisplayLink) {
        defer { last = link.timestamp }
        scroll(link)
        let late = (link.timestamp - last - link.duration) * 1000
        if last > 0, late > link.duration * 500 {
            try? out.write(contentsOf: Data("\(Date().timeIntervalSince1970) \(late)\n".utf8))
        }
    }
}
