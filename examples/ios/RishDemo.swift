import Foundation
import UIKit

private struct DemoPlanResponse: Decodable {
    struct Plan: Decodable {
        let kind: String
        let name: String?
    }

    let protocolVersion: UInt32
    let ok: Bool
    let plan: Plan?
    let error: String?

    private enum CodingKeys: String, CodingKey {
        case protocolVersion = "protocol_version"
        case ok, plan, error
    }
}

private struct DemoAppletResponse: Decodable {
    struct Outcome: Decodable {
        struct Path: Decodable {
            let kind: String
            let name: String?
        }

        let exitCode: Int32
        let stdout: [UInt8]
        let stderr: [UInt8]
        let path: Path

        private enum CodingKeys: String, CodingKey {
            case exitCode = "exit_code"
            case stdout, stderr, path
        }
    }

    let protocolVersion: UInt32
    let ok: Bool
    let outcome: Outcome?
    let error: String?

    private enum CodingKeys: String, CodingKey {
        case protocolVersion = "protocol_version"
        case ok, outcome, error
    }
}

private enum DemoFailure: Error, CustomStringConvertible {
    case invalidPlan(String)
    case invalidAppletResponse(String)
    case unexpectedStdout(String)

    var description: String {
        switch self {
        case let .invalidPlan(message):
            return "planner validation failed: \(message)"
        case let .invalidAppletResponse(message):
            return "applet validation failed: \(message)"
        case let .unexpectedStdout(stdout):
            return "unexpected echo stdout: \(stdout.debugDescription)"
        }
    }
}

private struct RishDemoReport {
    let runID: String
    let protocolVersion: UInt32
    let plannerResult: String
    let appletResult: String
    let exitCode: Int32?
    let succeeded: Bool
    let error: String?
    let log: String
}

private enum RishIOSDemo {
    static let expectedStdout = "hello-from-rish-ios\n"

    static func run() -> RishDemoReport {
        let runID = validatedRunID()
        var plannerResult = "not executed"
        var appletResult = "not executed"
        var exitCode: Int32?
        var failure: String?
        var lines = [
            "RISH_DEMO platform=ios-simulator",
            "RISH_DEMO protocol_version=\(RishBridge.protocolVersion)",
            "RISH_DEMO run_id=\(runID)",
        ]

        do {
            let planRequest: [String: Any] = [
                "platform": "ios",
                "privilege": "app_sandbox",
                "command": [
                    "program": "grep",
                    "args": ["needle"],
                    "env": [:],
                    "cwd": "/",
                    "stdin": [],
                ],
            ]
            let planRequestData = try JSONSerialization.data(withJSONObject: planRequest)
            let planData = try RishBridge.plan(request: planRequestData)
            let planned = try JSONDecoder().decode(DemoPlanResponse.self, from: planData)
            guard planned.protocolVersion == RishBridge.protocolVersion,
                  planned.ok,
                  planned.plan?.kind == "portable_applet",
                  planned.plan?.name == "grep"
            else {
                throw DemoFailure.invalidPlan(
                    planned.error ?? String(decoding: planData, as: UTF8.self)
                )
            }
            plannerResult = "portable_applet · grep"
            lines.append("RISH_DEMO planner.kind=portable_applet planner.name=grep")

            let container = URL(fileURLWithPath: NSHomeDirectory(), isDirectory: true)
            let configuration = try RishAppletConfiguration(
                sandboxRoot: container.appendingPathComponent(
                    "rish-demo-root",
                    isDirectory: true
                ),
                appContainerRoot: container
            )
            let command = RishGuestCommand(
                program: "echo",
                args: ["hello-from-rish-ios"]
            )
            let appletData = try RishBridge.executePortableApplet(
                command: command,
                configuration: configuration
            )
            let executed = try JSONDecoder().decode(
                DemoAppletResponse.self,
                from: appletData
            )
            guard executed.protocolVersion == RishBridge.protocolVersion,
                  executed.ok,
                  let outcome = executed.outcome,
                  outcome.exitCode == 0,
                  outcome.path.kind == "portable_applet",
                  outcome.path.name == "echo"
            else {
                throw DemoFailure.invalidAppletResponse(
                    executed.error ?? String(decoding: appletData, as: UTF8.self)
                )
            }
            let stdout = String(decoding: outcome.stdout, as: UTF8.self)
            guard stdout == expectedStdout else {
                throw DemoFailure.unexpectedStdout(stdout)
            }
            appletResult = stdout.trimmingCharacters(in: .newlines)
            exitCode = outcome.exitCode
            lines.append(
                "RISH_DEMO applet.kind=\(outcome.path.kind) applet.name=\(outcome.path.name ?? "")"
            )
            lines.append("RISH_DEMO applet.exit_code=\(outcome.exitCode)")
            lines.append("RISH_DEMO applet.stdout=\(appletResult)")
            lines.append("RISH_DEMO PASS")
        } catch {
            failure = String(describing: error)
            lines.append("RISH_DEMO error=\(error)")
            lines.append("RISH_DEMO FAIL")
        }

        return RishDemoReport(
            runID: runID,
            protocolVersion: RishBridge.protocolVersion,
            plannerResult: plannerResult,
            appletResult: appletResult,
            exitCode: exitCode,
            succeeded: failure == nil,
            error: failure,
            log: lines.joined(separator: "\n") + "\n"
        )
    }

    static func persist(_ report: RishDemoReport) {
        guard let documents = FileManager.default.urls(
            for: .documentDirectory,
            in: .userDomainMask
        ).first else {
            return
        }
        let resultURL = documents.appendingPathComponent("RishDemoResult.txt")
        try? Data(report.log.utf8).write(to: resultURL, options: .atomic)
    }

    private static func validatedRunID() -> String {
        let arguments = ProcessInfo.processInfo.arguments
        let candidate: String?
        if let flagIndex = arguments.firstIndex(of: "--rish-demo-run-id"),
           arguments.indices.contains(arguments.index(after: flagIndex)) {
            candidate = arguments[arguments.index(after: flagIndex)]
        } else {
            candidate = arguments.dropFirst().first
        }

        if let candidate,
           !candidate.isEmpty,
           candidate.utf8.count <= 96,
           candidate.unicodeScalars.allSatisfy({ scalar in
               let value = scalar.value
               return (48 ... 57).contains(value)
                   || (65 ... 90).contains(value)
                   || (97 ... 122).contains(value)
                   || value == 45 || value == 95
           }) {
            return candidate
        }
        return "ios-\(UUID().uuidString.lowercased())"
    }
}

private enum RishPalette {
    static let background = UIColor(red: 7 / 255, green: 17 / 255, blue: 31 / 255, alpha: 1)
    static let card = UIColor(red: 16 / 255, green: 28 / 255, blue: 45 / 255, alpha: 1)
    static let accent = UIColor(red: 94 / 255, green: 234 / 255, blue: 212 / 255, alpha: 1)
    static let success = UIColor(red: 126 / 255, green: 231 / 255, blue: 135 / 255, alpha: 1)
    static let secondary = UIColor(red: 148 / 255, green: 163 / 255, blue: 184 / 255, alpha: 1)
    static let primary = UIColor(red: 241 / 255, green: 245 / 255, blue: 249 / 255, alpha: 1)
    static let border = UIColor(red: 38 / 255, green: 56 / 255, blue: 78 / 255, alpha: 1)
}

private final class RishDemoViewController: UIViewController {
    private let report: RishDemoReport

    init(report: RishDemoReport) {
        self.report = report
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) is unavailable")
    }

    override var preferredStatusBarStyle: UIStatusBarStyle {
        .lightContent
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = RishPalette.background
        configureDashboard()
    }

    private func configureDashboard() {
        let scrollView = UIScrollView()
        scrollView.alwaysBounceVertical = true
        scrollView.showsVerticalScrollIndicator = false
        scrollView.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(scrollView)

        let content = UIStackView()
        content.axis = .vertical
        content.alignment = .fill
        content.spacing = 20
        content.translatesAutoresizingMaskIntoConstraints = false
        scrollView.addSubview(content)

        NSLayoutConstraint.activate([
            scrollView.topAnchor.constraint(equalTo: view.topAnchor),
            scrollView.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            scrollView.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            scrollView.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            content.topAnchor.constraint(
                equalTo: scrollView.contentLayoutGuide.topAnchor,
                constant: 20
            ),
            content.leadingAnchor.constraint(
                equalTo: scrollView.frameLayoutGuide.leadingAnchor,
                constant: 20
            ),
            content.trailingAnchor.constraint(
                equalTo: scrollView.frameLayoutGuide.trailingAnchor,
                constant: -20
            ),
            content.bottomAnchor.constraint(
                equalTo: scrollView.contentLayoutGuide.bottomAnchor,
                constant: -28
            ),
        ])

        content.addArrangedSubview(makeHeader())

        let title = makeLabel(
            "Linux tools,\nnative on mobile.",
            style: .largeTitle,
            weight: .bold,
            color: RishPalette.primary
        )
        title.minimumScaleFactor = 0.8
        content.addArrangedSubview(title)

        let subtitle = makeLabel(
            "Portable command semantics execute as native Rust, bounded by the app sandbox.",
            style: .body,
            weight: .regular,
            color: RishPalette.secondary
        )
        content.addArrangedSubview(subtitle)

        content.setCustomSpacing(28, after: subtitle)
        content.addArrangedSubview(
            makeSectionLabel("LIVE SESSION · \(shortRunID(report.runID))")
        )
        content.setCustomSpacing(10, after: content.arrangedSubviews.last!)

        content.addArrangedSubview(
            makeTerminalCard(
                title: "PLANNER",
                command: "plan grep needle",
                output: report.plannerResult,
                outputColor: report.succeeded ? RishPalette.accent : RishPalette.secondary
            )
        )
        content.setCustomSpacing(12, after: content.arrangedSubviews.last!)

        let appletOutput: String
        if let exitCode = report.exitCode {
            appletOutput = "\(report.appletResult)\nexit \(exitCode) · portable_applet"
        } else {
            appletOutput = report.error ?? report.appletResult
        }
        content.addArrangedSubview(
            makeTerminalCard(
                title: "EXECUTION",
                command: "echo hello-from-rish-ios",
                output: appletOutput,
                outputColor: report.succeeded ? RishPalette.success : .systemRed
            )
        )
        content.setCustomSpacing(12, after: content.arrangedSubviews.last!)

        content.addArrangedSubview(makeStatusCard())
        content.setCustomSpacing(26, after: content.arrangedSubviews.last!)

        let footer = makeLabel(
            "Portable applets  ·  App sandbox",
            style: .footnote,
            weight: .semibold,
            color: RishPalette.secondary,
            alignment: .center
        )
        content.addArrangedSubview(footer)
    }

    private func makeHeader() -> UIView {
        let container = UIStackView()
        container.axis = .vertical
        container.alignment = .fill
        container.spacing = 3

        let brandRow = UIStackView()
        brandRow.axis = .horizontal
        brandRow.alignment = .center
        brandRow.spacing = 12

        let brand = makeLabel(
            "rish",
            style: .title1,
            weight: .heavy,
            color: RishPalette.accent
        )
        brand.setContentCompressionResistancePriority(.required, for: .horizontal)
        brandRow.addArrangedSubview(brand)
        brandRow.addArrangedSubview(UIView())
        brandRow.addArrangedSubview(makePlatformPill())
        container.addArrangedSubview(brandRow)

        let tagline = makeLabel(
            "MOBILE LINUX RUNTIME",
            style: .caption1,
            weight: .bold,
            color: RishPalette.secondary
        )
        tagline.attributedText = trackedText(
            "MOBILE LINUX RUNTIME",
            font: tagline.font,
            color: RishPalette.secondary,
            tracking: 1.7
        )
        container.addArrangedSubview(tagline)
        return container
    }

    private func makePlatformPill() -> UIView {
        let pill = UIView()
        pill.backgroundColor = RishPalette.accent.withAlphaComponent(0.1)
        pill.layer.borderColor = RishPalette.accent.withAlphaComponent(0.32).cgColor
        pill.layer.borderWidth = 1
        pill.layer.cornerRadius = 14

        let label = makeLabel(
            "iOS SIMULATOR  /  ABI v\(report.protocolVersion)",
            style: .caption2,
            weight: .bold,
            color: RishPalette.accent
        )
        label.adjustsFontSizeToFitWidth = true
        label.minimumScaleFactor = 0.75
        label.translatesAutoresizingMaskIntoConstraints = false
        pill.addSubview(label)
        NSLayoutConstraint.activate([
            label.topAnchor.constraint(equalTo: pill.topAnchor, constant: 7),
            label.leadingAnchor.constraint(equalTo: pill.leadingAnchor, constant: 11),
            label.trailingAnchor.constraint(equalTo: pill.trailingAnchor, constant: -11),
            label.bottomAnchor.constraint(equalTo: pill.bottomAnchor, constant: -7),
        ])
        return pill
    }

    private func makeSectionLabel(_ text: String) -> UILabel {
        let label = makeLabel(
            text,
            style: .caption1,
            weight: .bold,
            color: RishPalette.secondary
        )
        label.attributedText = trackedText(
            text,
            font: label.font,
            color: RishPalette.secondary,
            tracking: 1.4
        )
        return label
    }

    private func makeTerminalCard(
        title: String,
        command: String,
        output: String,
        outputColor: UIColor
    ) -> UIView {
        let card = makeCard()
        let stack = UIStackView()
        stack.axis = .vertical
        stack.alignment = .fill
        stack.spacing = 13
        stack.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(stack)

        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: card.topAnchor, constant: 17),
            stack.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 17),
            stack.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -17),
            stack.bottomAnchor.constraint(equalTo: card.bottomAnchor, constant: -17),
        ])

        let chrome = UIStackView()
        chrome.axis = .horizontal
        chrome.alignment = .center
        chrome.spacing = 6
        chrome.addArrangedSubview(makeDot(color: UIColor(red: 1, green: 0.42, blue: 0.42, alpha: 1)))
        chrome.addArrangedSubview(makeDot(color: UIColor(red: 1, green: 0.78, blue: 0.35, alpha: 1)))
        chrome.addArrangedSubview(makeDot(color: RishPalette.success))
        chrome.addArrangedSubview(UIView())
        chrome.addArrangedSubview(
            makeLabel(
                title,
                style: .caption2,
                weight: .bold,
                color: RishPalette.secondary
            )
        )
        stack.addArrangedSubview(chrome)

        let commandLabel = makeMonospacedLabel(style: .body, color: RishPalette.primary)
        let commandText = NSMutableAttributedString(
            string: "$ ",
            attributes: [
                .font: commandLabel.font as Any,
                .foregroundColor: RishPalette.accent,
            ]
        )
        commandText.append(
            NSAttributedString(
                string: command,
                attributes: [
                    .font: commandLabel.font as Any,
                    .foregroundColor: RishPalette.primary,
                ]
            )
        )
        commandLabel.attributedText = commandText
        stack.addArrangedSubview(commandLabel)

        let outputLabel = makeMonospacedLabel(style: .callout, color: outputColor)
        outputLabel.text = "↳ \(output)"
        stack.addArrangedSubview(outputLabel)
        return card
    }

    private func makeStatusCard() -> UIView {
        let card = makeCard()
        card.backgroundColor = report.succeeded
            ? RishPalette.success.withAlphaComponent(0.08)
            : UIColor.systemRed.withAlphaComponent(0.08)
        card.layer.borderColor = (
            report.succeeded ? RishPalette.success : UIColor.systemRed
        ).withAlphaComponent(0.35).cgColor

        let stack = UIStackView()
        stack.axis = .horizontal
        stack.alignment = .center
        stack.spacing = 12
        stack.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: card.topAnchor, constant: 17),
            stack.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 17),
            stack.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -17),
            stack.bottomAnchor.constraint(equalTo: card.bottomAnchor, constant: -17),
        ])

        stack.addArrangedSubview(
            makeStatusGlyph(succeeded: report.succeeded)
        )
        let copy = UIStackView()
        copy.axis = .vertical
        copy.alignment = .leading
        copy.spacing = 3
        copy.addArrangedSubview(
            makeLabel(
                report.succeeded ? "PASS" : "CHECK FAILED",
                style: .headline,
                weight: .bold,
                color: report.succeeded ? RishPalette.success : .systemRed
            )
        )
        copy.addArrangedSubview(
            makeLabel(
                report.succeeded
                    ? "Planner and applet execution verified"
                    : (report.error ?? "Unknown verification error"),
                style: .caption1,
                weight: .regular,
                color: RishPalette.secondary
            )
        )
        stack.addArrangedSubview(copy)
        return card
    }

    private func makeCard() -> UIView {
        let card = UIView()
        card.backgroundColor = RishPalette.card
        card.layer.cornerRadius = 18
        card.layer.borderWidth = 1
        card.layer.borderColor = RishPalette.border.cgColor
        return card
    }

    private func makeDot(color: UIColor) -> UIView {
        let dot = UIView()
        dot.backgroundColor = color
        dot.layer.cornerRadius = 4
        NSLayoutConstraint.activate([
            dot.widthAnchor.constraint(equalToConstant: 8),
            dot.heightAnchor.constraint(equalToConstant: 8),
        ])
        return dot
    }

    private func makeStatusGlyph(succeeded: Bool) -> UIView {
        let glyph = UILabel()
        glyph.text = succeeded ? "✓" : "!"
        glyph.textAlignment = .center
        glyph.textColor = RishPalette.background
        glyph.backgroundColor = succeeded ? RishPalette.success : .systemRed
        glyph.font = .systemFont(ofSize: 17, weight: .black)
        glyph.layer.cornerRadius = 17
        glyph.clipsToBounds = true
        NSLayoutConstraint.activate([
            glyph.widthAnchor.constraint(equalToConstant: 34),
            glyph.heightAnchor.constraint(equalToConstant: 34),
        ])
        return glyph
    }

    private func makeLabel(
        _ text: String,
        style: UIFont.TextStyle,
        weight: UIFont.Weight,
        color: UIColor,
        alignment: NSTextAlignment = .natural
    ) -> UILabel {
        let label = UILabel()
        let descriptor = UIFontDescriptor.preferredFontDescriptor(withTextStyle: style)
        label.font = UIFont.systemFont(ofSize: descriptor.pointSize, weight: weight)
        label.adjustsFontForContentSizeCategory = true
        label.text = text
        label.textColor = color
        label.textAlignment = alignment
        label.numberOfLines = 0
        label.lineBreakMode = .byWordWrapping
        return label
    }

    private func makeMonospacedLabel(
        style: UIFont.TextStyle,
        color: UIColor
    ) -> UILabel {
        let label = UILabel()
        let descriptor = UIFontDescriptor.preferredFontDescriptor(withTextStyle: style)
        let base = UIFont.monospacedSystemFont(
            ofSize: descriptor.pointSize,
            weight: .medium
        )
        label.font = UIFontMetrics(forTextStyle: style).scaledFont(for: base)
        label.adjustsFontForContentSizeCategory = true
        label.textColor = color
        label.numberOfLines = 0
        label.lineBreakMode = .byCharWrapping
        return label
    }

    private func trackedText(
        _ text: String,
        font: UIFont,
        color: UIColor,
        tracking: CGFloat
    ) -> NSAttributedString {
        NSAttributedString(
            string: text,
            attributes: [
                .font: font,
                .foregroundColor: color,
                .kern: tracking,
            ]
        )
    }

    private func shortRunID(_ runID: String) -> String {
        runID.count > 28 ? "\(runID.prefix(25))…" : runID
    }
}

@main
@MainActor
final class RishDemoAppDelegate: UIResponder, UIApplicationDelegate {
    var window: UIWindow?

    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [
            UIApplication.LaunchOptionsKey: Any
        ]? = nil
    ) -> Bool {
        let report = RishIOSDemo.run()
        print(report.log, terminator: "")
        RishIOSDemo.persist(report)

        let pullController = RishImagePullViewController(
            service: RishImagePullClientAdapter()
        )
        pullController.tabBarItem = UITabBarItem(
            title: "Pull",
            image: UIImage(systemName: "shippingbox.and.arrow.backward"),
            selectedImage: UIImage(
                systemName: "shippingbox.and.arrow.backward.fill"
            )
        )

        let runtimeController = RishDemoViewController(report: report)
        runtimeController.tabBarItem = UITabBarItem(
            title: "Runtime",
            image: UIImage(systemName: "terminal"),
            selectedImage: UIImage(systemName: "terminal.fill")
        )

        let tabs = UITabBarController()
        tabs.viewControllers = [pullController, runtimeController]
        tabs.selectedIndex = 0
        tabs.tabBar.tintColor = RishPalette.accent
        tabs.tabBar.unselectedItemTintColor = RishPalette.secondary
        let appearance = UITabBarAppearance()
        appearance.configureWithOpaqueBackground()
        appearance.backgroundColor = RishPalette.card
        appearance.shadowColor = RishPalette.border
        tabs.tabBar.standardAppearance = appearance
        tabs.tabBar.scrollEdgeAppearance = appearance

        let appWindow = UIWindow(frame: UIScreen.main.bounds)
        appWindow.rootViewController = tabs
        appWindow.makeKeyAndVisible()
        window = appWindow
        return true
    }
}
