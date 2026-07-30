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

private enum RishIOSDemo {
    static let bundleIdentifier = "dev.rish.demo"
    static let expectedStdout = "hello-from-rish-ios\n"

    static func run() -> String {
        var lines = [
            "RISH_DEMO platform=ios-simulator",
            "RISH_DEMO run_id=\(runIdentifier())",
            "RISH_DEMO protocol_version=\(RishBridge.protocolVersion)",
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
            lines.append(
                "RISH_DEMO applet.kind=\(outcome.path.kind) applet.name=\(outcome.path.name ?? "")"
            )
            lines.append("RISH_DEMO applet.exit_code=\(outcome.exitCode)")
            lines.append("RISH_DEMO applet.stdout=\(stdout.trimmingCharacters(in: .newlines))")
            lines.append("RISH_DEMO PASS")
        } catch {
            lines.append("RISH_DEMO error=\(error)")
            lines.append("RISH_DEMO FAIL")
        }

        return lines.joined(separator: "\n") + "\n"
    }

    private static func runIdentifier() -> String {
        let arguments = CommandLine.arguments
        guard let flag = arguments.firstIndex(of: "--rish-demo-run-id"),
              arguments.indices.contains(flag + 1)
        else {
            return "interactive"
        }
        let value = arguments[flag + 1]
        let allowed = CharacterSet.alphanumerics.union(
            CharacterSet(charactersIn: "-")
        )
        guard !value.isEmpty,
              value.count <= 64,
              value.unicodeScalars.allSatisfy(allowed.contains)
        else {
            return "invalid"
        }
        return value
    }

    static func persist(_ result: String) {
        guard let documents = FileManager.default.urls(
            for: .documentDirectory,
            in: .userDomainMask
        ).first else {
            return
        }
        let resultURL = documents.appendingPathComponent("RishDemoResult.txt")
        try? Data(result.utf8).write(to: resultURL, options: .atomic)
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
        let result = RishIOSDemo.run()
        print(result, terminator: "")
        RishIOSDemo.persist(result)

        let label = UILabel(frame: .zero)
        label.numberOfLines = 0
        label.font = .monospacedSystemFont(ofSize: 12, weight: .regular)
        label.text = result

        let controller = UIViewController()
        controller.view.backgroundColor = .systemBackground
        controller.view.addSubview(label)
        label.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([
            label.leadingAnchor.constraint(
                equalTo: controller.view.safeAreaLayoutGuide.leadingAnchor,
                constant: 16
            ),
            label.trailingAnchor.constraint(
                equalTo: controller.view.safeAreaLayoutGuide.trailingAnchor,
                constant: -16
            ),
            label.centerYAnchor.constraint(equalTo: controller.view.centerYAnchor),
        ])

        let appWindow = UIWindow(frame: UIScreen.main.bounds)
        appWindow.rootViewController = controller
        appWindow.makeKeyAndVisible()
        window = appWindow
        return true
    }
}
