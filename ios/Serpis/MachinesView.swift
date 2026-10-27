import SwiftUI

/// The machines this device reaches, and how to let it into another one.
struct MachinesView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var adding = false
    @State private var copied = false
    @State private var scanning = false

    var body: some View {
        NavigationStack {
            List {
                Section {
                    if CodeScanner.available {
                        Button { scanning = true } label: { Label("Scan your computer's code", systemImage: "qrcode.viewfinder") }
                    }
                } header: {
                    Text("Pair with your computer")
                } footer: {
                    Text("In serpis on your computer, run phone-pair, then scan the code it shows (the Camera app works too). This phone gets the machines you share there, and is let in once you say yes.")
                }
                Section {
                    ForEach(app.machines) { m in
                        MachineRow(machine: m, link: app.link(m.id))
                            .swipeActions { Button("Remove", role: .destructive) { app.remove(m) } }
                        if case .unknownHost(let fingerprint, let changed) = app.link(m.id) {
                            HostKeyCheck(machine: m, fingerprint: fingerprint, changed: changed)
                        }
                    }
                    Button { adding = true } label: { Label("Add a machine", systemImage: "plus") }
                }
                Section {
                    Text(app.publicKey)
                        .font(.system(.caption, design: .monospaced))
                        .textSelection(.enabled)
                        .lineLimit(3)
                    Button {
                        UIPasteboard.general.string = app.publicKey
                        copied = true
                    } label: {
                        Label(copied ? "Copied" : "Copy this device's key", systemImage: copied ? "checkmark" : "doc.on.doc")
                    }
                } header: {
                    Text("This device")
                } footer: {
                    Text("Add this line to ~/.ssh/authorized_keys on a machine to let this device in. It stays on this device.")
                }
            }
            .navigationTitle("Machines")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
            .sheet(isPresented: $adding) { AddMachineView() }
            .fullScreenCover(isPresented: $scanning) {
                NavigationStack {
                    CodeScanner { link in
                        scanning = false
                        dismiss()
                        app.pair(link, label: UIDevice.current.name)
                    }
                    .ignoresSafeArea()
                    .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cancel") { scanning = false } } }
                }
            }
        }
    }
}

struct MachineRow: View {
    let machine: Machine
    let link: Link

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "desktopcomputer")
                .foregroundStyle(.secondary)
                .frame(width: 24)
            VStack(alignment: .leading, spacing: 2) {
                Text(machine.name).font(.body.weight(.medium))
                Text(detail).font(.caption).foregroundStyle(.secondary).lineLimit(2)
            }
            Spacer()
            switch link {
            case .connecting: ProgressView()
            case .online: Circle().fill(.green).frame(width: 8, height: 8)
            case .unknownHost: Image(systemName: "lock.shield").foregroundStyle(.orange)
            case .offline(let why): if why != nil { Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange) }
            }
        }
        .contentShape(Rectangle())
    }

    private var detail: String {
        if case .offline(let why?) = link { return why }
        return machine.address
    }
}

/// A machine's host key, for the person to check before this device trusts it.
struct HostKeyCheck: View {
    @Environment(AppModel.self) private var app
    let machine: Machine
    let fingerprint: String
    let changed: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(changed ? "\(machine.name)'s key changed" : "First time reaching \(machine.name)")
                .font(.subheadline.weight(.semibold))
            Text(changed
                 ? "The machine shows a different key from the one you accepted. If it wasn't reinstalled, someone may be in the middle."
                 : "Check it shows this key (ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub there):")
                .font(.caption)
                .foregroundStyle(.secondary)
            Text(fingerprint).font(.system(.caption, design: .monospaced)).textSelection(.enabled)
            Button(changed ? "Trust the new key" : "Trust it") {
                Task { await app.accept(machine, fingerprint: fingerprint) }
            }
            .buttonStyle(.bordered)
            .tint(changed ? .red : .accentColor)
        }
        .padding(.vertical, 4)
    }
}

struct AddMachineView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var address = ""

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("me@box.example.com", text: $address)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .keyboardType(.URL)
                        .accessibilityIdentifier("address")
                    TextField("Name (optional)", text: $name)
                } footer: {
                    Text("user@host, or user@host:port. The machine needs serpis set up: open a chat there once from serpis on a computer.")
                }
            }
            .navigationTitle("Add a machine")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Add") {
                        let m = app.add(name: name.trimmingCharacters(in: .whitespaces), address: address.trimmingCharacters(in: .whitespaces))
                        dismiss()
                        Task { await app.connect(m); await app.refresh() }
                    }
                    .disabled(!address.contains("@"))
                }
            }
        }
    }
}
