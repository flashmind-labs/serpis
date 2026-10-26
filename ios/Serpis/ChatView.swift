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

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 20) {
                    let items = Item.of(chat.entries)
                    ForEach(items) { item in
                        switch item {
                        case .entry(_, let entry): EntryView(entry: entry)
                        case .work(_, let entries): WorkView(entries: entries, live: chat.running && item.id == items.last?.id)
                        }
                    }
                    if chat.running, case .some(.user) = chat.entries.last {
                        Working()
                    }
                    Color.clear.frame(height: 1).id("end")
                }
                .padding(.horizontal, 18)
                .padding(.top, 12)
                .padding(.bottom, 8)
            }
            .scrollDismissesKeyboard(.interactively)
            .accessibilityIdentifier("transcript")
            .defaultScrollAnchor(.bottom)
            .onChange(of: chat.entries) { _, _ in
                withAnimation(.easeOut(duration: 0.2)) { proxy.scrollTo("end", anchor: .bottom) }
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
        }
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

    private var composer: some View {
        VStack(spacing: 8) {
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
