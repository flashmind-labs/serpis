import SwiftUI
import VisionKit

/// How pairing with a desktop goes: waiting for its yes (with this phone's key, which the
/// desktop asks about), paired, or why not.
struct PairingView: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        NavigationStack {
            VStack(spacing: 16) {
                switch app.pairing {
                case .waiting(let fingerprint):
                    ProgressView().controlSize(.large).padding(.bottom, 8)
                    Text("Pairing with your computer").font(.title3.weight(.semibold))
                    Text("Say yes there. It asks about this phone's key:")
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                    Text(fingerprint)
                        .font(.system(.footnote, design: .monospaced))
                        .multilineTextAlignment(.center)
                        .textSelection(.enabled)
                        .accessibilityIdentifier("pairing-fingerprint")
                case .paired(let names):
                    Image(systemName: "checkmark.circle.fill").font(.system(size: 56)).foregroundStyle(.green)
                    Text("Paired").font(.title3.weight(.semibold))
                    Text("This phone can reach \(names.formatted(.list(type: .and))).")
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                case .failed(let why):
                    Image(systemName: "xmark.octagon.fill").font(.system(size: 56)).foregroundStyle(.red)
                    Text("Couldn't pair").font(.title3.weight(.semibold))
                    Text(why).foregroundStyle(.secondary).multilineTextAlignment(.center)
                case nil:
                    EmptyView()
                }
            }
            .padding(32)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button(app.pairing?.isWaiting == true ? "Hide" : "Done") { app.dismissPairing() }
                }
            }
        }
        .presentationDetents([.medium])
    }
}

/// The camera, reading the code `phone-pair` shows (where the device has a scanner).
struct CodeScanner: UIViewControllerRepresentable {
    let found: (String) -> Void

    @MainActor static var available: Bool { DataScannerViewController.isSupported && DataScannerViewController.isAvailable }

    func makeUIViewController(context: Context) -> DataScannerViewController {
        let scanner = DataScannerViewController(recognizedDataTypes: [.barcode(symbologies: [.qr])], qualityLevel: .balanced, isHighlightingEnabled: true)
        scanner.delegate = context.coordinator
        return scanner
    }

    func updateUIViewController(_ scanner: DataScannerViewController, context: Context) {
        if !scanner.isScanning { try? scanner.startScanning() }
    }

    func makeCoordinator() -> Coordinator { Coordinator(found: found) }

    final class Coordinator: NSObject, DataScannerViewControllerDelegate {
        let found: (String) -> Void
        private var done = false

        init(found: @escaping (String) -> Void) { self.found = found }

        func dataScanner(_ scanner: DataScannerViewController, didAdd added: [RecognizedItem], allItems: [RecognizedItem]) {
            for case .barcode(let code) in added {
                if let link = code.payloadStringValue, link.hasPrefix("serpis://pair#"), !done {
                    done = true
                    found(link)
                }
            }
        }
    }
}
