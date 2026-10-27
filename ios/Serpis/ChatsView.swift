import SwiftUI

/// Every chat on every machine: the ones working first, then the latest.
struct ChatsView: View {
    @Environment(AppModel.self) private var app
    @State private var search = ""
    @State private var path: [ChatModel] = []
    @State private var newChat = false
    @State private var machines = false

    var body: some View {
        NavigationStack(path: $path) {
            List {
                problems
                ForEach(Self.sections(filtered), id: \.title) { section in
                    Section {
                        ForEach(section.rows) { row in
                            Button {
                                path.append(app.chat(row.machine, row.chat, path: row.path))
                            } label: {
                                ChatRowView(row: row, machine: app.machine(row.machine)?.name ?? "")
                            }
                            .buttonStyle(.plain)
                            .accessibilityIdentifier("chat-row")
                        }
                    } header: {
                        Text(section.title)
                            .font(.footnote.weight(.semibold))
                            .foregroundStyle(.secondary)
                            .textCase(nil)
                    }
                }
            }
            .listStyle(.plain)
            .overlay { empty }
            .navigationTitle("Chats")
            .searchable(text: $search, prompt: "Search chats")
            .refreshable { await app.refresh() }
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button { machines = true } label: { Image(systemName: "desktopcomputer") }
                        .accessibilityLabel("Machines")
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button { newChat = true } label: { Image(systemName: "square.and.pencil") }
                        .accessibilityLabel("New chat")
                        .accessibilityIdentifier("new-chat")
                        .disabled(app.machines.isEmpty)
                }
            }
            .navigationDestination(for: ChatModel.self) { ChatView(chat: $0) }
            .sheet(isPresented: $newChat) {
                NewChatView { chat in
                    newChat = false
                    path.append(chat)
                }
            }
            .sheet(isPresented: $machines) { MachinesView() }
            .task { await app.refresh() }
        }
    }

    /// The rows by when: working now, then today, yesterday, this week, and before.
    static func sections(_ rows: [ChatRow], now: Date = .now) -> [(title: String, rows: [ChatRow])] {
        let cal = Calendar.current
        let today = cal.startOfDay(for: now)
        let yesterday = cal.date(byAdding: .day, value: -1, to: today)!
        let week = cal.date(byAdding: .day, value: -7, to: today)!
        var out: [(title: String, rows: [ChatRow])] = [("Working", []), ("Today", []), ("Yesterday", []), ("Previous 7 days", []), ("Older", [])]
        for r in rows {
            let i = r.running ? 0 : r.modified >= today ? 1 : r.modified >= yesterday ? 2 : r.modified >= week ? 3 : 4
            out[i].rows.append(r)
        }
        return out.filter { !$0.rows.isEmpty }
    }

    private var filtered: [ChatRow] {
        let q = search.trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { return app.rows }
        return app.rows.filter { $0.title.localizedCaseInsensitiveContains(q) || $0.cwd.localizedCaseInsensitiveContains(q) }
    }

    /// Machines that couldn't be reached, said once at the top.
    @ViewBuilder private var problems: some View {
        ForEach(app.machines) { m in
            switch app.link(m.id) {
            case .offline(let why?):
                Label { Text("\(m.name): \(why)").font(.footnote) } icon: { Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange) }
                    .listRowSeparator(.hidden)
            case .unknownHost:
                Button { machines = true } label: {
                    Label("Confirm \(m.name)'s key to reach it", systemImage: "lock.shield").font(.footnote)
                }
                .listRowSeparator(.hidden)
            default:
                EmptyView()
            }
        }
    }

    @ViewBuilder private var empty: some View {
        if app.machines.isEmpty {
            ContentUnavailableView {
                Label("No machines yet", systemImage: "desktopcomputer")
            } description: {
                Text("Run phone-pair in serpis on your computer and scan its code with the Camera, or add a machine you reach over SSH. Its chats show here.")
            } actions: {
                Button("Add a machine") { machines = true }.buttonStyle(.borderedProminent)
            }
        } else if app.rows.isEmpty && !app.loading {
            ContentUnavailableView {
                Label(search.isEmpty ? "No chats yet" : "No chats match", systemImage: "bubble.left.and.bubble.right")
            } description: {
                Text(search.isEmpty ? "Start one on any of your machines." : "")
            } actions: {
                if search.isEmpty { Button("New chat") { newChat = true }.buttonStyle(.borderedProminent) }
            }
        } else if app.rows.isEmpty {
            ProgressView()
        }
    }
}

struct ChatRowView: View {
    let row: ChatRow
    let machine: String

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(alignment: .leading, spacing: 3) {
                Text(row.title)
                    .font(.body.weight(.medium))
                    .lineLimit(2)
                HStack(spacing: 4) {
                    if row.running {
                        Pulse()
                        Text(row.step?.isEmpty == false ? row.step! : "Working")
                    } else {
                        Text(row.folder.isEmpty ? machine : "\(row.folder) · \(machine)")
                    }
                }
                .font(.subheadline)
                .foregroundStyle(row.running ? AnyShapeStyle(Color.accentColor) : AnyShapeStyle(.secondary))
                .lineLimit(1)
            }
            Spacer(minLength: 8)
            if row.modified > .distantPast {
                Text(row.modified, format: .relative(presentation: .numeric, unitsStyle: .abbreviated))
                    .font(.caption)
                    .foregroundStyle(.tertiary)
            }
        }
        .padding(.vertical, 4)
        .contentShape(Rectangle())
    }
}

/// A chat at work: a dot breathing.
struct Pulse: View {
    @State private var on = false
    var body: some View {
        Circle()
            .fill(Color.accentColor)
            .frame(width: 7, height: 7)
            .opacity(on ? 1 : 0.35)
            .animation(.easeInOut(duration: 0.8).repeatForever(autoreverses: true), value: on)
            .onAppear { on = true }
    }
}
