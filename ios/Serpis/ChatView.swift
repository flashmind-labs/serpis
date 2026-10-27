import PhotosUI
import SwiftUI

/// A chat: its transcript, live, and the composer under it.
struct ChatView: View {
    @Environment(AppModel.self) private var app
    @Bindable var chat: ChatModel
    @State private var draft = ""
    @State private var picked: [PhotosPickerItem] = []
    @State private var images: [SentImage] = []
    @State private var sending = false
    @FocusState private var typing: Bool
    /// Where the transcript is scrolled: it follows new text only while it is at the bottom.
    @State private var position = ScrollPosition(edge: .bottom)
    @State private var atBottom = true

    var body: some View {
        ZStack(alignment: .bottom) {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 20) {
                    let items = Item.of(chat.entries)
                    ForEach(items) { item in
                        switch item {
                        case .entry(_, let entry): EntryView(entry: entry)
                        case .work(_, let entries): WorkView(entries: entries, live: chat.running && item.id == items.last?.id)
                        }
                    }
                    if chat.running, ChatView.waitingOnReply(chat.entries.last) {
                        Working(thinking: chat.state?.thinkingTokens)
                    }
                }
                .padding(.horizontal, 18)
                .padding(.top, 12)
                .padding(.bottom, 8)
            }
            .scrollDismissesKeyboard(.interactively)
            // A tap on the transcript puts the keyboard away, as in any chat.
            .simultaneousGesture(TapGesture().onEnded { typing = false })
            .accessibilityIdentifier("transcript")
            .defaultScrollAnchor(.bottom)
            .scrollPosition($position)
            .onScrollGeometryChange(for: Bool.self) { g in
                g.contentOffset.y + g.containerSize.height >= g.contentSize.height - 48
            } action: { _, bottom in
                atBottom = bottom
            }
            // New text is followed while you're at the bottom; scrolled up to read, nothing moves.
            .onChange(of: chat.entries) { _, _ in
                if atBottom { position.scrollTo(edge: .bottom) }
            }
            .overlay {
                if chat.entries.isEmpty && !chat.running {
                    ContentUnavailableView {
                        Label(app.machine(chat.machine)?.name ?? "", systemImage: "desktopcomputer")
                    } description: {
                        Text(chat.state.map { ($0.cwd as NSString).abbreviatingWithTildeInPath } ?? "")
                    }
                    .foregroundStyle(.secondary)
                }
            }
            if !atBottom {
                Button {
                    withAnimation(.snappy) { position.scrollTo(edge: .bottom) }
                } label: {
                    Image(systemName: "arrow.down")
                        .font(.system(size: 15, weight: .semibold))
                        .frame(width: 38, height: 38)
                        .background(.regularMaterial, in: Circle())
                        .overlay(Circle().strokeBorder(Color.hairline, lineWidth: 0.5))
                        .shadow(color: .black.opacity(0.08), radius: 8, y: 2)
                }
                .buttonStyle(.plain)
                .padding(.bottom, 10)
                .transition(.scale(scale: 0.8).combined(with: .opacity))
                .accessibilityLabel("Scroll to the bottom")
            }
        }
        .animation(.snappy, value: atBottom)
        .safeAreaInset(edge: .bottom) { composer }
        .navigationTitle(chat.title)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                VStack(spacing: 0) {
                    Text(chat.title).font(.headline).lineLimit(1)
                    Text(subtitle).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
            }
            ToolbarItem(placement: .topBarTrailing) { modelMenu }
        }
        .task { await chat.attach() }
        .onDisappear { chat.detach() }
        .alert("Couldn't do that", isPresented: Binding(get: { chat.notice != nil }, set: { if !$0 { chat.notice = nil } })) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(chat.notice ?? "")
        }
    }

    private var subtitle: String {
        let machine = app.machine(chat.machine)?.name ?? ""
        if chat.running, let step = chat.state?.step, !step.isEmpty { return step }
        if !chat.connected { return "\(machine) · reconnecting…" }
        let folder = chat.state.map { ($0.cwd as NSString).lastPathComponent } ?? ""
        return folder.isEmpty ? machine : "\(folder) · \(machine)"
    }

    private var modelMenu: some View {
        Menu {
            ForEach(chat.models) { m in
                Button {
                    Task { await chat.setModel(m.model) }
                } label: {
                    if m.model == chat.model { Label(m.name, systemImage: "checkmark") } else { Text(m.name) }
                }
            }
        } label: {
            Text(chat.modelName)
                .font(.subheadline)
                .lineLimit(1)
        }
        .disabled(chat.models.isEmpty || chat.running)
        .accessibilityLabel("Model")
    }

    /// A tool call waiting for you: what it wants, and the answers.
    @ViewBuilder private var approval: some View {
        if let tool = chat.waiting, tool.name == "ExitPlanMode" {
            PlanApproval(plan: tool.arg("plan")) { decision in
                switch decision {
                case .approve(let mode): answer(tool, "plan", mode: mode)
                case .keepPlanning(let what): answer(tool, "deny", message: what)
                }
            }
            .id(tool.id)
        } else if let tool = chat.waiting, tool.name == "AskUserQuestion" {
            QuestionsForm(tool: tool) { answers in
                answer(tool, answers == nil ? "deny" : "answers", answers: answers)
            }
            .id(tool.id)
        } else if let tool = chat.waiting {
            VStack(alignment: .leading, spacing: 10) {
                Label("Allow \(tool.doing.prefix(1).lowercased() + tool.doing.dropFirst())?", systemImage: "hand.raised")
                    .font(.subheadline.weight(.medium))
                    .lineLimit(2)
                HStack(spacing: 8) {
                    Button("Deny", role: .destructive) { answer(tool, "deny") }
                        .buttonStyle(.bordered)
                    if tool.ask?.always != nil {
                        Button("Always") { answer(tool, "always") }
                            .buttonStyle(.bordered)
                            .accessibilityHint(tool.ask?.always.map { "Also allows \($0) from now on" } ?? "")
                    }
                    Spacer()
                    Button("Allow") { answer(tool, "allow") }
                        .buttonStyle(.borderedProminent)
                }
            }
            .padding(12)
            .background(.quaternary.opacity(0.6), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
            .accessibilityIdentifier("approval")
        }
    }

    /// Answer what `tool` asks, and follow what it does next, as after sending.
    private func answer(_ tool: Tool, _ kind: String, answers: [String: String]? = nil, mode: String? = nil, message: String? = nil) {
        withAnimation(.snappy) { position.scrollTo(edge: .bottom) }
        Task { await chat.answer(tool.id, kind, answers: answers, mode: mode, message: message) }
    }

    /// Claude Code's permission mode, as its prompt line shows it, and a menu to switch it.
    @ViewBuilder private var permissionMode: some View {
        if let mode = chat.state?.permissionMode {
            let label = permissionModes.first { $0.mode == mode }?.label ?? mode
            Menu {
                ForEach(permissionModes, id: \.mode) { m in
                    Button {
                        Task { await chat.setPermissionMode(m.mode) }
                    } label: {
                        if m.mode == mode { Label(m.label, systemImage: "checkmark") } else { Text(m.label) }
                        Text(m.about)
                    }
                }
            } label: {
                Text(label)
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(modeColor(mode))
            }
            .accessibilityLabel("Permission mode: \(label)")
            .accessibilityIdentifier("permission-mode")
        }
    }

    private func modeColor(_ mode: String) -> Color {
        switch mode {
        case "acceptEdits": .purple
        case "plan": .teal
        case "auto", "dontAsk": .orange
        case "bypassPermissions": .red
        default: .secondary
        }
    }

    /// What runs on in the background, and a way to stop it.
    @ViewBuilder private var backgroundWork: some View {
        let running = chat.background
        if !running.isEmpty {
            HStack(spacing: 8) {
                ProgressView().controlSize(.mini)
                Text(running.count == 1 ? (running[0].side.task?.description ?? "1 task") : ChatView.counted(running))
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                Text("in the background").font(.footnote).foregroundStyle(.tertiary)
                Spacer()
                Menu("Stop") {
                    ForEach(running, id: \.id) { t in
                        Button(t.side.task?.description ?? t.name, role: .destructive) { Task { await chat.stopTask(t) } }
                    }
                }
                .font(.footnote)
            }
            .padding(.horizontal, 6)
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("background")
        }
    }

    /// Whether the transcript's end shows nothing of the reply yet: your message, or a reply
    /// that so far only thinks (its thinking not shown).
    static func waitingOnReply(_ last: Entry?) -> Bool {
        switch last {
        case .user: true
        case .assistant(let blocks, let streaming, _):
            streaming && !blocks.contains { if case .text(let t) = $0 { !t.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty } else { false } }
        default: false
        }
    }

    /// What runs in the background, counted by kind, as Claude Code counts it: "1 shell · 1 monitor".
    static func counted(_ tools: [Tool]) -> String {
        var counts: [(one: String, many: String, n: Int)] = []
        for t in tools {
            let (one, many) = switch t.side.task?.kind ?? "" {
            case "bash": ("shell", "shells")
            case "monitor": ("monitor", "monitors")
            case "agent", "remote_agent", "in_process_teammate": ("agent", "agents")
            default: ("task", "tasks")
            }
            if let i = counts.firstIndex(where: { $0.one == one }) { counts[i].n += 1 } else { counts.append((one, many, 1)) }
        }
        let order = ["shell", "monitor", "agent", "task"]
        return counts.sorted { (order.firstIndex(of: $0.one) ?? 9) < (order.firstIndex(of: $1.one) ?? 9) }
            .map { $0.n == 1 ? "1 \($0.one)" : "\($0.n) \($0.many)" }.joined(separator: " · ")
    }

    private var composer: some View {
        VStack(spacing: 8) {
            if chat.state?.permissionMode != nil || !chat.background.isEmpty {
                HStack(spacing: 10) {
                    permissionMode
                    Spacer(minLength: 0)
                }
                .padding(.horizontal, 6)
            }
            backgroundWork
            approval
            if !images.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack {
                        ForEach(Array(images.enumerated()), id: \.offset) { i, image in
                            if let data = Data(base64Encoded: image.data), let ui = UIImage(data: data) {
                                Image(uiImage: ui).resizable().scaledToFill()
                                    .frame(width: 56, height: 56)
                                    .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
                                    .overlay(alignment: .topTrailing) {
                                        Button { images.remove(at: i) } label: {
                                            Image(systemName: "xmark.circle.fill").symbolRenderingMode(.palette).foregroundStyle(.white, .black.opacity(0.6))
                                        }
                                        .offset(x: 6, y: -6)
                                    }
                            }
                        }
                    }
                    .padding(.top, 6)
                }
            }
            HStack(alignment: .bottom, spacing: 10) {
                PhotosPicker(selection: $picked, maxSelectionCount: 4, matching: .images) {
                    Image(systemName: "plus")
                        .font(.system(size: 17, weight: .semibold))
                        .frame(width: 36, height: 36)
                        .background(.quaternary, in: Circle())
                }
                .accessibilityLabel("Add photos")
                .onChange(of: picked) { _, items in Task { await load(items) } }
                TextField(chat.running ? "Steer it…" : "Message", text: $draft, axis: .vertical)
                    .lineLimit(1...6)
                    .focused($typing)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 9)
                    .background(.quaternary.opacity(0.6), in: RoundedRectangle(cornerRadius: 20, style: .continuous))
                    .accessibilityIdentifier("composer")
                button
            }
        }
        .padding(.horizontal, 12)
        .padding(.top, 6)
        .padding(.bottom, 8)
        .background(.bar)
    }

    @ViewBuilder private var button: some View {
        let empty = draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && images.isEmpty
        if chat.running && empty {
            Button { Task { await chat.stop() } } label: {
                Image(systemName: "stop.fill")
                    .font(.system(size: 14, weight: .bold))
                    .frame(width: 36, height: 36)
                    .background(Color.primary, in: Circle())
                    .foregroundStyle(Color(.systemBackground))
            }
            .accessibilityLabel("Stop")
        } else {
            Button(action: send) {
                Image(systemName: "arrow.up")
                    .font(.system(size: 16, weight: .bold))
                    .frame(width: 36, height: 36)
                    .background(empty ? AnyShapeStyle(.quaternary) : AnyShapeStyle(Color.accentColor), in: Circle())
                    .foregroundStyle(empty ? AnyShapeStyle(.secondary) : AnyShapeStyle(.white))
            }
            .disabled(empty || sending || !chat.connected)
            .accessibilityLabel("Send")
        }
    }

    private func send() {
        let (text, attached) = (draft.trimmingCharacters(in: .whitespacesAndNewlines), images)
        draft = ""
        images = []
        picked = []
        sending = true
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
        withAnimation(.snappy) { position.scrollTo(edge: .bottom) }
        Task {
            if !(await chat.send(text, images: attached)) {
                // Refused: what was written is still there to send again.
                draft = text
                images = attached
            }
            sending = false
        }
    }

    /// Photos picked, as the model takes them: JPEG, at most 2048 points on a side.
    private func load(_ items: [PhotosPickerItem]) async {
        var out: [SentImage] = []
        for item in items {
            guard let data = try? await item.loadTransferable(type: Data.self), let image = UIImage(data: data) else { continue }
            let scale = min(1, 2048 / max(image.size.width, image.size.height))
            let size = CGSize(width: image.size.width * scale, height: image.size.height * scale)
            let resized = UIGraphicsImageRenderer(size: size).image { _ in image.draw(in: CGRect(origin: .zero, size: size)) }
            if let jpeg = resized.jpegData(compressionQuality: 0.85) {
                out.append(SentImage(mime: "image/jpeg", data: jpeg.base64EncodedString()))
            }
        }
        images = out
    }
}

/// The questions a tool asks (Claude Code's AskUserQuestion), as a form: an option for each
/// (several where it takes several), or your own words; Send when each has an answer.
struct QuestionsForm: View {
    let tool: Tool
    /// The answers (question to answer), or nil for Skip.
    let done: ([String: String]?) -> Void
    @State private var picked: [String: Set<String>] = [:]
    @State private var own: [String: String] = [:]

    private func answer(_ q: Question) -> String? {
        let typed = (own[q.text] ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        if !typed.isEmpty { return typed }
        let chosen = q.options.map(\.label).filter { picked[q.text]?.contains($0) == true }
        return chosen.isEmpty ? nil : chosen.joined(separator: ", ")
    }

    var body: some View {
        let questions = tool.questions
        VStack(alignment: .leading, spacing: 10) {
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    ForEach(questions, id: \.text) { q in
                        VStack(alignment: .leading, spacing: 6) {
                            if !q.header.isEmpty {
                                Text(q.header.uppercased()).font(.caption2.weight(.semibold)).foregroundStyle(.secondary)
                            }
                            Text(q.text).font(.subheadline.weight(.medium))
                            ForEach(q.options, id: \.label) { o in
                                let on = picked[q.text]?.contains(o.label) == true
                                Button {
                                    var set = q.multi ? (picked[q.text] ?? []) : []
                                    if on { set.remove(o.label) } else { set.insert(o.label) }
                                    picked[q.text] = set
                                    own[q.text] = ""
                                } label: {
                                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                                        Image(systemName: on ? (q.multi ? "checkmark.square.fill" : "largecircle.fill.circle") : (q.multi ? "square" : "circle"))
                                            .foregroundStyle(on ? Color.accentColor : .secondary)
                                        VStack(alignment: .leading, spacing: 1) {
                                            Text(o.label).foregroundStyle(.primary)
                                            if !o.detail.isEmpty {
                                                Text(o.detail).font(.caption).foregroundStyle(.secondary)
                                            }
                                        }
                                        Spacer(minLength: 0)
                                    }
                                    .contentShape(Rectangle())
                                }
                                .buttonStyle(.plain)
                                .accessibilityLabel(o.label)
                                .accessibilityAddTraits(on ? .isSelected : [])
                            }
                            TextField("Or your own answer", text: Binding(get: { own[q.text] ?? "" }, set: { own[q.text] = $0; if !$0.isEmpty { picked[q.text] = [] } }))
                                .font(.subheadline)
                                .textFieldStyle(.roundedBorder)
                        }
                    }
                }
            }
            .frame(maxHeight: 320)
            .fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 8) {
                Button("Skip", role: .destructive) { done(nil) }
                    .buttonStyle(.bordered)
                Spacer()
                let answers = Dictionary(uniqueKeysWithValues: questions.compactMap { q in answer(q).map { (q.text, $0) } })
                Button("Send answers") { done(answers) }
                    .buttonStyle(.borderedProminent)
                    .disabled(answers.count < questions.count)
            }
        }
        .padding(12)
        .background(.quaternary.opacity(0.6), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("questions")
    }
}

/// A plan Claude Code asks to carry out: what it is, and approving it (on asking before edits,
/// or accepting them) or planning on with what should change.
struct PlanApproval: View {
    enum Decision { case approve(String), keepPlanning(String?) }
    let plan: String
    let done: (Decision) -> Void
    @State private var asking = false
    @State private var change = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label("Approve the plan?", systemImage: "list.bullet.clipboard")
                .font(.subheadline.weight(.medium))
            ScrollView {
                Text((try? AttributedString(markdown: plan, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace))) ?? AttributedString(plan))
                    .font(.footnote)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(maxHeight: 220)
            .fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 8) {
                Button("Keep planning") { asking = true }
                    .buttonStyle(.bordered)
                Spacer(minLength: 4)
                Button("Accept edits") { done(.approve("acceptEdits")) }
                    .buttonStyle(.bordered)
                Button("Approve") { done(.approve("default")) }
                    .buttonStyle(.borderedProminent)
            }
            .lineLimit(1)
            .fixedSize(horizontal: false, vertical: true)
            .controlSize(.small)
        }
        .padding(12)
        .background(.quaternary.opacity(0.6), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("plan")
        .alert("What should change?", isPresented: $asking) {
            TextField("Say what to change", text: $change)
            Button("Cancel", role: .cancel) {}
            Button("Plan on") { done(.keepPlanning(change.isEmpty ? nil : change)) }
        }
    }
}
