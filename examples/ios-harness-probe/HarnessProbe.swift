import Foundation
import UIKit
import CryptoKit
import SafariServices

/// A separate, credential-free device experiment. Every command executes in
/// the bundled Linux guest; the host never spawns a Harness process.
@main
final class HarnessProbe: UIResponder, UIApplicationDelegate {
    var window: UIWindow?

    func application(_ application: UIApplication,
                     didFinishLaunchingWithOptions options: [UIApplication.LaunchOptionsKey: Any]?) -> Bool {
        let window = UIWindow(frame: UIScreen.main.bounds)
        window.rootViewController = ProbeController()
        window.makeKeyAndVisible()
        self.window = window
        return true
    }
}

final class ProbeController: UIViewController {
    private let output = UITextView()
    private let signIn = UIButton(type: .system)
    private let queue = DispatchQueue(label: "dev.zseven.rish.harness-probe")
    private let started = Date()
    private let runID = UUID().uuidString
    private var records: [[String: Any]] = []
    private var streamBuffers: [UInt32: Data] = [:]
    private var streamSequence: UInt64 = 0
    private var streamProtocolFailed = false

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .systemBackground
        output.isEditable = false
        output.dataDetectorTypes = [.link]
        output.font = .monospacedSystemFont(ofSize: 13, weight: .regular)
        output.translatesAutoresizingMaskIntoConstraints = false
        signIn.setTitle("Open official Codex sign-in", for: .normal)
        signIn.isEnabled = false
        signIn.translatesAutoresizingMaskIntoConstraints = false
        signIn.addTarget(self, action: #selector(openSignIn), for: .touchUpInside)
        view.addSubview(output)
        view.addSubview(signIn)
        NSLayoutConstraint.activate([
            output.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor),
            output.bottomAnchor.constraint(equalTo: signIn.topAnchor),
            output.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 12),
            output.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -12),
            signIn.bottomAnchor.constraint(equalTo: view.safeAreaLayoutGuide.bottomAnchor),
            signIn.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 12),
            signIn.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -12),
            signIn.heightAnchor.constraint(equalToConstant: 48),
        ])
        queue.async { self.runProbe() }
    }

    @objc private func openSignIn() {
        // Fixed official destination; process output cannot choose a host.
        guard let url = URL(string: "https://auth.openai.com/codex/device") else { return }
        present(SFSafariViewController(url: url), animated: true)
    }

    private func record(_ stage: String, _ details: [String: Any] = [:]) {
        let item: [String: Any] = ["stage": stage, "elapsed_seconds": Date().timeIntervalSince(started),
                                  "details": details]
        records.append(item)
        let document: [String: Any] = ["schema_version": 1, "run_id": runID,
            "host_os": ProcessInfo.processInfo.operatingSystemVersionString,
            "host_pid": ProcessInfo.processInfo.processIdentifier,
            "execution": "on-device-probe", "records": records]
        if let data = try? JSONSerialization.data(withJSONObject: document, options: [.prettyPrinted, .sortedKeys]),
           let directory = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first {
            try? data.write(to: directory.appendingPathComponent("harness-probe.json"), options: .atomic)
        }
        let serialized = (try? JSONSerialization.data(withJSONObject: details, options: [.prettyPrinted, .sortedKeys]))
            .flatMap { String(data: $0, encoding: .utf8) } ?? ""
        let detail = details["text"] as? String ?? serialized
        let line = "[\(Int(Date().timeIntervalSince(started)))s] \(stage)\n\(detail)\n\n"
        DispatchQueue.main.async {
            self.output.text += line
            self.output.scrollRangeToVisible(NSRange(location: self.output.text.utf16.count, length: 0))
            if detail.contains("https://auth.openai.com/codex/device") {
                self.signIn.isEnabled = true
            }
        }
    }

    private func runProbe() {
        guard let kernel = Bundle.main.path(forResource: "kernel", ofType: nil),
              let initrd = Bundle.main.path(forResource: "harness", ofType: "cpio"),
              let configurationURL = Bundle.main.url(forResource: "probe", withExtension: "json"),
              let configurationData = try? Data(contentsOf: configurationURL),
              let configuration = (try? JSONSerialization.jsonObject(with: configurationData)) as? [String: Any] else {
            record("invalid-bundle")
            return
        }
        if let downloads = configuration["downloads"] as? [[String: String]] {
            downloadFixtures(downloads)
            return
        }
        guard
              let commands = configuration["commands"] as? [[String]],
              !commands.isEmpty, commands.count <= 8 else {
            record("invalid-bundle")
            return
        }
        let request: [String: Any] = [
            "kernel_path": kernel, "initrd_path": initrd, "command": ["true"],
            "memory_mib": configuration["memory_mib"] as? Int ?? 1024,
            "network": configuration["network"] as? String == "user-nat" ? "user-nat" : "disabled",
            "command_line": "console=ttyS0,115200n8 rdinit=/init panic=-1 oops=panic nokaslr cgroup_no_v1=all 8250.nr_uarts=1",
            "boot_budget_units": 60_000_000_000 as UInt64,
            "handshake_budget_units": 40_000_000_000 as UInt64,
        ]
        record("boot-start", ["configuration": configuration])
        guard let data = try? JSONSerialization.data(withJSONObject: request),
              let text = String(data: data, encoding: .utf8) else { return }
        let session = text.withCString { rish_vm_boot_session($0, text.utf8.count) }
        guard let session else {
            record("boot-failed")
            return
        }
        defer { rish_vm_session_free(session) }
        record("boot-ready")
        for command in commands {
            guard !command.isEmpty,
                  let data = try? JSONSerialization.data(withJSONObject: ["command": command]),
                  let text = String(data: data, encoding: .utf8) else { continue }
            record("command-start", ["argv": command])
            streamSequence = 0
            streamProtocolFailed = false
            let context = Unmanaged.passUnretained(self).toOpaque()
            let raw = text.withCString {
                rish_vm_session_exec_stream_json(session, $0, text.utf8.count, context) { context, bytes, length in
                    guard let context, let bytes, length > 0 else { return }
                    let controller = Unmanaged<ProbeController>.fromOpaque(context).takeUnretainedValue()
                    controller.receiveEnvelope(Data(bytes: bytes, count: length))
                }
            }
            for (channel, bytes) in streamBuffers where !bytes.isEmpty {
                record(channel == 2 ? "stderr" : "stdout", ["text": String(decoding: bytes, as: UTF8.self)])
            }
            streamBuffers.removeAll()
            guard let raw else {
                record("command-failed", ["error": "null response"])
                return
            }
            let responseText = String(cString: raw)
            rish_string_free(raw)
            let response = responseText.data(using: .utf8).flatMap {
                try? JSONSerialization.jsonObject(with: $0)
            }
            record("command-result", ["argv": command, "response": response ?? responseText])
            if streamProtocolFailed { return }
            guard let result = response as? [String: Any],
                  result["ok"] as? Bool == true,
                  result["exit_code"] as? Int == 0 else { return }
        }
        record("probe-finished")
    }

    private func receiveEnvelope(_ data: Data) {
        guard !streamProtocolFailed else { return }
        guard let event = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
              event["protocol_version"] as? Int == 1, event["event"] as? String == "output",
              let sequence = event["sequence"] as? UInt64, sequence == streamSequence,
              let channel = event["channel"] as? String, ["stdout", "stderr", "console"].contains(channel),
              let encoded = event["data_base64"] as? String, let bytes = Data(base64Encoded: encoded) else {
            streamProtocolFailed = true
            record("output-protocol-error")
            return
        }
        streamSequence += 1
        receiveOutput(channel == "stderr" ? 2 : channel == "console" ? 3 : 1, bytes)
    }

    private func receiveOutput(_ channel: UInt32, _ bytes: Data) {
        var pending = streamBuffers[channel] ?? Data()
        pending.append(bytes)
        while let newline = pending.firstIndex(of: 10) {
            let line = pending.prefix(through: newline)
            record(channel == 2 ? "stderr" : "stdout", ["text": String(decoding: line, as: UTF8.self)])
            pending.removeSubrange(...newline)
        }
        // Keep partial UTF-8 lines intact between guest frames.
        if pending.count > 65536 {
            record("output-line-truncated", ["channel": channel, "bytes": pending.count])
            pending.removeAll()
        }
        streamBuffers[channel] = pending
    }

    /// Downloads test inputs only. No downloaded code executes on the host.
    private func downloadFixtures(_ downloads: [[String: String]]) {
        guard downloads.count <= 2,
              let directory = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first else {
            record("invalid-download-list")
            return
        }
        for item in downloads {
            guard let name = item["name"], ["codex.tgz", "claude.tgz"].contains(name),
                  let urlText = item["url"], let url = URL(string: urlText),
                  url.scheme == "https", url.host == "registry.npmjs.org",
                  url.user == nil, url.password == nil,
                  let expected = item["sha512"] else {
                record("invalid-download")
                return
            }
            record("fixture-download-start", ["name": name, "url": urlText])
            let semaphore = DispatchSemaphore(value: 0)
            let config = URLSessionConfiguration.ephemeral
            config.timeoutIntervalForRequest = 60
            config.timeoutIntervalForResource = 300
            let session = URLSession(configuration: config)
            let task = session.downloadTask(with: url) { temporary, response, error in
                defer { semaphore.signal() }
                guard let temporary, (response as? HTTPURLResponse)?.statusCode == 200 else {
                    self.record("fixture-download-failed", ["name": name,
                        "error": error?.localizedDescription ?? "non-200 response"])
                    return
                }
                do {
                    let data = try Data(contentsOf: temporary, options: .mappedIfSafe)
                    guard data.count <= 384 * 1024 * 1024,
                          Data(SHA512.hash(data: data)).base64EncodedString() == expected else {
                        self.record("fixture-integrity-failed", ["name": name])
                        return
                    }
                    let destination = directory.appendingPathComponent(name)
                    try data.write(to: destination, options: [.atomic, .completeFileProtection])
                    self.record("fixture-verified", ["name": name, "bytes": data.count,
                        "sha256": SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()])
                } catch {
                    self.record("fixture-download-failed", ["name": name, "error": error.localizedDescription])
                }
            }
            task.resume()
            semaphore.wait()
            session.invalidateAndCancel()
        }
        record("fixture-downloads-finished")
    }
}
