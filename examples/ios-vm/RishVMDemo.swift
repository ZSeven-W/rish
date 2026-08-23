import Foundation
import UIKit

/// An interactive terminal into an x86-64 Linux guest booted by the pure-Rust
/// interpreter. The guest boots once; every command the user types runs over
/// the same open control channel.
enum RishVMConfig {
    static let initrdResource = "rish-container"
    static let commandLine =
        "console=ttyS0,115200n8 rdinit=/init panic=-1 oops=panic nokaslr cgroup_no_v1=all 8250.nr_uarts=1"

    static func bootRequest() -> RishBridge.RishVMRunRequest? {
        guard let kernel = Bundle.main.path(forResource: "vmlinuz-virt-6.18.35", ofType: nil),
              let initrd = Bundle.main.path(forResource: initrdResource, ofType: "cpio") else {
            return nil
        }
        return RishBridge.RishVMRunRequest(
            kernelPath: kernel,
            initrdPath: initrd,
            command: ["true"],
            memoryMib: 1024,
            commandLine: commandLine,
            bootBudgetUnits: 60_000_000_000,
            handshakeBudgetUnits: 40_000_000_000
        )
    }
}

// MARK: - Palette

private enum Theme {
    static let bgTop = UIColor(red: 0.04, green: 0.06, blue: 0.13, alpha: 1)
    static let bgBottom = UIColor(red: 0.02, green: 0.03, blue: 0.07, alpha: 1)
    static let terminal = UIColor(red: 0.02, green: 0.04, blue: 0.08, alpha: 1)
    static let inputBar = UIColor(red: 0.07, green: 0.10, blue: 0.18, alpha: 1)
    static let border = UIColor(red: 0.16, green: 0.22, blue: 0.34, alpha: 1)
    static let accent = UIColor(red: 0.37, green: 0.92, blue: 0.83, alpha: 1)
    static let accent2 = UIColor(red: 0.53, green: 0.66, blue: 1.0, alpha: 1)
    static let success = UIColor(red: 0.49, green: 0.90, blue: 0.53, alpha: 1)
    static let failure = UIColor(red: 0.97, green: 0.45, blue: 0.45, alpha: 1)
    static let primary = UIColor(red: 0.94, green: 0.96, blue: 0.99, alpha: 1)
    static let secondary = UIColor(red: 0.58, green: 0.64, blue: 0.75, alpha: 1)
    static let prompt = UIColor(red: 0.37, green: 0.92, blue: 0.83, alpha: 1)
    static let output = UIColor(red: 0.80, green: 0.88, blue: 0.97, alpha: 1)
}

// MARK: - Terminal view controller

final class RishTerminalViewController: UIViewController, UITextFieldDelegate {
    private let gradient = CAGradientLayer()
    private let transcript = UITextView()
    private let promptLabel = UILabel()
    private let input = UITextField()
    private let runButton = UIButton(type: .system)
    private let statusPill = PaddedLabel()
    private let spinner = UIActivityIndicatorView(style: .medium)
    private var inputBarBottom: NSLayoutConstraint!

    private let work = DispatchQueue(label: "dev.rish.vm", qos: .userInitiated)
    private var session: RishVMSession?
    private var booting = true
    private var startTime = Date()
    /// The guest working directory, tracked on the host: every command runs a
    /// fresh `sh`, so `cd` cannot persist in the guest. We `cd` into this dir
    /// before each command and read `pwd` back to keep it current.
    private var cwd = "/"

    override func viewDidLoad() {
        super.viewDidLoad()
        buildUI()
        bootGuest()
        NotificationCenter.default.addObserver(
            self, selector: #selector(keyboardWillChange(_:)),
            name: UIResponder.keyboardWillChangeFrameNotification, object: nil)
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        gradient.frame = view.bounds
    }

    // MARK: UI

    private func buildUI() {
        gradient.colors = [Theme.bgTop.cgColor, Theme.bgBottom.cgColor]
        view.layer.insertSublayer(gradient, at: 0)

        let header = makeHeader()
        let terminalCard = makeTerminalCard()
        let inputBar = makeInputBar()
        for v in [header, terminalCard, inputBar] {
            v.translatesAutoresizingMaskIntoConstraints = false
            view.addSubview(v)
        }
        inputBarBottom = inputBar.bottomAnchor.constraint(
            equalTo: view.safeAreaLayoutGuide.bottomAnchor, constant: -10)
        NSLayoutConstraint.activate([
            header.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor, constant: 12),
            header.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 20),
            header.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -20),

            terminalCard.topAnchor.constraint(equalTo: header.bottomAnchor, constant: 14),
            terminalCard.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 14),
            terminalCard.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -14),
            terminalCard.bottomAnchor.constraint(equalTo: inputBar.topAnchor, constant: -12),

            inputBar.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 14),
            inputBar.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -14),
            inputBarBottom,
        ])
    }

    private func makeHeader() -> UIView {
        let title = UILabel()
        let text = NSMutableAttributedString(string: "rish", attributes: [
            .font: UIFont.systemFont(ofSize: 26, weight: .heavy), .foregroundColor: Theme.primary,
        ])
        text.append(NSAttributedString(string: " ●", attributes: [
            .font: UIFont.systemFont(ofSize: 26, weight: .heavy), .foregroundColor: Theme.accent,
        ]))
        title.attributedText = text

        let subtitle = UILabel()
        subtitle.text = "an x86-64 Linux container, on iOS"
        subtitle.font = .systemFont(ofSize: 13, weight: .medium)
        subtitle.textColor = Theme.secondary

        statusPill.text = "booting…"
        statusPill.font = .systemFont(ofSize: 11, weight: .bold)
        statusPill.textColor = Theme.accent2
        statusPill.backgroundColor = Theme.accent2.withAlphaComponent(0.14)
        statusPill.layer.cornerRadius = 9
        statusPill.insets = UIEdgeInsets(top: 4, left: 10, bottom: 4, right: 10)
        statusPill.setContentHuggingPriority(.required, for: .horizontal)
        statusPill.setContentCompressionResistancePriority(.required, for: .horizontal)

        spinner.color = Theme.accent
        spinner.startAnimating()
        spinner.setContentHuggingPriority(.required, for: .horizontal)

        let left = UIStackView(arrangedSubviews: [title, subtitle])
        left.axis = .vertical
        left.spacing = 2
        let row = UIStackView(arrangedSubviews: [left, spinner, statusPill])
        row.axis = .horizontal
        row.spacing = 8
        row.alignment = .center
        return row
    }

    private func makeTerminalCard() -> UIView {
        let card = UIView()
        card.backgroundColor = Theme.terminal
        card.layer.cornerRadius = 16
        card.layer.borderWidth = 1
        card.layer.borderColor = Theme.border.cgColor
        card.clipsToBounds = true

        let dots = UIStackView()
        dots.axis = .horizontal
        dots.spacing = 7
        for color in [UIColor.systemRed, .systemYellow, .systemGreen] {
            let d = UIView()
            d.backgroundColor = color.withAlphaComponent(0.85)
            d.layer.cornerRadius = 5
            d.translatesAutoresizingMaskIntoConstraints = false
            d.widthAnchor.constraint(equalToConstant: 10).isActive = true
            d.heightAnchor.constraint(equalToConstant: 10).isActive = true
            dots.addArrangedSubview(d)
        }
        let barTitle = UILabel()
        barTitle.text = "root@rish-container"
        barTitle.font = .monospacedSystemFont(ofSize: 11, weight: .medium)
        barTitle.textColor = Theme.secondary
        let titleBar = UIStackView(arrangedSubviews: [dots, barTitle, UIView()])
        titleBar.axis = .horizontal
        titleBar.spacing = 10
        titleBar.alignment = .center
        titleBar.translatesAutoresizingMaskIntoConstraints = false

        transcript.backgroundColor = .clear
        transcript.isEditable = false
        transcript.font = .monospacedSystemFont(ofSize: 12.5, weight: .regular)
        transcript.textColor = Theme.output
        transcript.textContainerInset = UIEdgeInsets(top: 8, left: 4, bottom: 8, right: 4)
        transcript.translatesAutoresizingMaskIntoConstraints = false

        let sep = UIView()
        sep.backgroundColor = Theme.border
        sep.translatesAutoresizingMaskIntoConstraints = false

        card.addSubview(titleBar)
        card.addSubview(sep)
        card.addSubview(transcript)
        NSLayoutConstraint.activate([
            titleBar.topAnchor.constraint(equalTo: card.topAnchor, constant: 12),
            titleBar.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 14),
            titleBar.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -14),
            sep.topAnchor.constraint(equalTo: titleBar.bottomAnchor, constant: 10),
            sep.leadingAnchor.constraint(equalTo: card.leadingAnchor),
            sep.trailingAnchor.constraint(equalTo: card.trailingAnchor),
            sep.heightAnchor.constraint(equalToConstant: 1),
            transcript.topAnchor.constraint(equalTo: sep.bottomAnchor),
            transcript.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 10),
            transcript.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -10),
            transcript.bottomAnchor.constraint(equalTo: card.bottomAnchor, constant: -8),
        ])
        return card
    }

    private func makeInputBar() -> UIView {
        let bar = UIView()
        bar.backgroundColor = Theme.inputBar
        bar.layer.cornerRadius = 14
        bar.layer.borderWidth = 1
        bar.layer.borderColor = Theme.border.cgColor

        promptLabel.text = "$"
        promptLabel.font = .monospacedSystemFont(ofSize: 16, weight: .bold)
        promptLabel.textColor = Theme.prompt

        input.font = .monospacedSystemFont(ofSize: 14, weight: .regular)
        input.textColor = Theme.primary
        input.tintColor = Theme.accent
        input.autocapitalizationType = .none
        input.autocorrectionType = .no
        input.spellCheckingType = .no
        input.smartQuotesType = .no
        input.keyboardType = .asciiCapable
        input.returnKeyType = .go
        input.delegate = self
        input.isEnabled = false
        input.attributedPlaceholder = NSAttributedString(
            string: "booting the guest…",
            attributes: [.foregroundColor: Theme.secondary])

        var config = UIButton.Configuration.filled()
        config.title = "Run"
        config.baseBackgroundColor = Theme.accent
        config.baseForegroundColor = Theme.bgBottom
        config.cornerStyle = .medium
        config.contentInsets = NSDirectionalEdgeInsets(top: 8, leading: 16, bottom: 8, trailing: 16)
        config.titleTextAttributesTransformer = UIConfigurationTextAttributesTransformer { attr in
            var attr = attr
            attr.font = .systemFont(ofSize: 15, weight: .bold)
            return attr
        }
        runButton.configuration = config
        runButton.isEnabled = false
        runButton.alpha = 0.5
        runButton.addTarget(self, action: #selector(runTapped), for: .touchUpInside)
        runButton.setContentHuggingPriority(.required, for: .horizontal)

        let row = UIStackView(arrangedSubviews: [promptLabel, input, runButton])
        row.axis = .horizontal
        row.spacing = 10
        row.alignment = .center
        row.translatesAutoresizingMaskIntoConstraints = false
        bar.addSubview(row)
        NSLayoutConstraint.activate([
            row.topAnchor.constraint(equalTo: bar.topAnchor, constant: 8),
            row.leadingAnchor.constraint(equalTo: bar.leadingAnchor, constant: 14),
            row.trailingAnchor.constraint(equalTo: bar.trailingAnchor, constant: -10),
            row.bottomAnchor.constraint(equalTo: bar.bottomAnchor, constant: -8),
        ])
        return bar
    }

    // MARK: Boot + run

    private func bootGuest() {
        append(line: "booting an x86-64 Linux guest in a pure-Rust interpreter…\n",
               color: Theme.secondary)
        guard let request = RishVMConfig.bootRequest() else {
            append(line: "error: bundled kernel/initrd not found\n", color: Theme.failure)
            statusReady(false)
            return
        }
        startTime = Date()
        work.async { [weak self] in
            let session = RishVMSession.boot(request)
            DispatchQueue.main.async {
                guard let self else { return }
                self.booting = false
                self.session = session
                if session != nil {
                    let secs = Date().timeIntervalSince(self.startTime)
                    self.append(
                        line: "guest is up in \(String(format: "%.0f", secs))s — real x86-64 Linux, isolated from iOS\n",
                        color: Theme.success)
                    self.append(
                        line: "try:  cat /proc/cpuinfo  ·  ls /  ·  ps  ·  echo hi\n\n",
                        color: Theme.secondary)
                    self.statusReady(true)
                    self.execute("uname -a")
                } else {
                    self.append(line: "the guest failed to boot\n", color: Theme.failure)
                    self.statusReady(false)
                }
            }
        }
    }

    private func statusReady(_ ok: Bool) {
        spinner.stopAnimating()
        statusPill.text = ok ? "live" : "failed"
        statusPill.textColor = ok ? Theme.success : Theme.failure
        statusPill.backgroundColor = (ok ? Theme.success : Theme.failure).withAlphaComponent(0.16)
        input.isEnabled = ok
        runButton.isEnabled = ok
        runButton.alpha = ok ? 1 : 0.5
        input.attributedPlaceholder = NSAttributedString(
            string: ok ? "type a shell command…" : "guest unavailable",
            attributes: [.foregroundColor: Theme.secondary])
        if ok { input.becomeFirstResponder() }
    }

    @objc private func runTapped() { submit() }

    func textFieldShouldReturn(_ textField: UITextField) -> Bool {
        submit()
        return false
    }

    private func submit() {
        guard let text = input.text?.trimmingCharacters(in: .whitespaces), !text.isEmpty else {
            return
        }
        input.text = ""
        execute(text)
    }

    /// Record separator (0x1E): brackets the `pwd` we append after each command
    /// so the host can recover the guest's new working directory and strip the
    /// marker from what the user sees.
    private static let cwdMark = "\u{1E}"

    private func execute(_ text: String) {
        guard let session else { return }
        append(line: "\(cwd) $ \(text)\n", color: Theme.prompt)
        setBusy(true)
        let dir = cwd
        work.async { [weak self] in
            // Each command runs in a fresh sandbox, so mount the pseudo
            // filesystems every time (idempotent, silenced) — this is what makes
            // ps / free / df / cat /proc/* work. Then run inside the tracked dir
            // and report the resulting `pwd` so a `cd` in `text` sticks.
            let mark = RishTerminalViewController.cwdMark
            let setup = "mount -t proc proc /proc 2>/dev/null;"
                + "mount -t sysfs sysfs /sys 2>/dev/null"
            let script = "\(setup)\ncd '\(dir)' 2>/dev/null\n\(text)\n__rish_rc=$?\n"
                + "printf '\(mark)%s\(mark)' \"$(pwd)\"\nexit $__rish_rc"
            let result = try? session.run(["sh", "-lc", script])
            DispatchQueue.main.async {
                guard let self else { return }
                if let result {
                    var out = result.stdout ?? ""
                    if let dir = self.takeCwd(from: &out), !dir.isEmpty { self.cwd = dir }
                    let err = result.stderr ?? ""
                    if !out.isEmpty { self.append(line: out, color: Theme.output) }
                    if !err.isEmpty { self.append(line: err, color: Theme.failure) }
                    if out.isEmpty && err.isEmpty {
                        self.append(line: "(no output)\n", color: Theme.secondary)
                    } else if !(out + err).hasSuffix("\n") {
                        self.append(line: "\n", color: Theme.output)
                    }
                    if let code = result.exitCode, code != 0 {
                        self.append(line: "[exit \(code)]\n", color: Theme.secondary)
                    }
                } else {
                    self.append(line: "command failed\n", color: Theme.failure)
                }
                self.setBusy(false)
            }
        }
    }

    /// Pulls the trailing `\u{1E}pwd\u{1E}` marker out of `out`, removing it from
    /// the visible output and returning the guest's new working directory.
    private func takeCwd(from out: inout String) -> String? {
        let mark = RishTerminalViewController.cwdMark
        guard let first = out.range(of: mark),
              let second = out.range(of: mark, range: first.upperBound..<out.endIndex)
        else { return nil }
        let dir = String(out[first.upperBound..<second.lowerBound])
        out.removeSubrange(first.lowerBound..<second.upperBound)
        return dir
    }

    private func setBusy(_ busy: Bool) {
        input.isEnabled = !busy
        runButton.isEnabled = !busy
        runButton.alpha = busy ? 0.5 : 1
        if busy {
            spinner.startAnimating()
        } else {
            spinner.stopAnimating()
            input.becomeFirstResponder()
        }
    }

    private func append(line: String, color: UIColor) {
        let attr = NSMutableAttributedString(
            attributedString: transcript.attributedText ?? NSAttributedString())
        attr.append(NSAttributedString(string: line, attributes: [
            .font: UIFont.monospacedSystemFont(ofSize: 12.5, weight: .regular),
            .foregroundColor: color,
        ]))
        transcript.attributedText = attr
        transcript.scrollRangeToVisible(NSRange(location: attr.length, length: 0))
        persistTranscript()
    }

    /// Mirrors the transcript to the app's Documents so the run script can
    /// confirm the guest booted and the first command ran.
    private func persistTranscript() {
        guard let documents = FileManager.default.urls(
            for: .documentDirectory, in: .userDomainMask).first else { return }
        let text = transcript.attributedText?.string ?? ""
        try? Data(text.utf8).write(
            to: documents.appendingPathComponent("RishVMResult.txt"), options: .atomic)
    }

    @objc private func keyboardWillChange(_ note: Notification) {
        guard let frame = note.userInfo?[UIResponder.keyboardFrameEndUserInfoKey]
            as? NSValue else { return }
        let kb = view.convert(frame.cgRectValue, from: nil)
        let overlap = max(0, view.bounds.height - kb.minY - view.safeAreaInsets.bottom)
        inputBarBottom.constant = -(overlap + 10)
        view.layoutIfNeeded()
    }
}

/// A label with content insets, for pills.
final class PaddedLabel: UILabel {
    var insets = UIEdgeInsets(top: 4, left: 10, bottom: 4, right: 10)
    override func drawText(in rect: CGRect) { super.drawText(in: rect.inset(by: insets)) }
    override var intrinsicContentSize: CGSize {
        let s = super.intrinsicContentSize
        return CGSize(width: s.width + insets.left + insets.right,
                      height: s.height + insets.top + insets.bottom)
    }
}

@main
@MainActor
final class RishVMDemoAppDelegate: UIResponder, UIApplicationDelegate {
    var window: UIWindow?
    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [
            UIApplication.LaunchOptionsKey: Any
        ]? = nil
    ) -> Bool {
        let window = UIWindow(frame: UIScreen.main.bounds)
        window.rootViewController = RishTerminalViewController()
        window.makeKeyAndVisible()
        self.window = window
        return true
    }
}
