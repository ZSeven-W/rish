import Foundation

#if RISH_STANDALONE_TYPECHECK
@_silgen_name("rish_plan_json")
private func rish_plan_json(_ input: UnsafePointer<CChar>, _ count: Int) -> UnsafeMutablePointer<CChar>?
@_silgen_name("rish_execute_applet_json")
private func rish_execute_applet_json(_ input: UnsafePointer<CChar>, _ count: Int) -> UnsafeMutablePointer<CChar>?
@_silgen_name("rish_vm_run_docker_json")
private func rish_vm_run_docker_json(_ input: UnsafePointer<CChar>, _ count: Int) -> UnsafeMutablePointer<CChar>?
@_silgen_name("rish_vm_boot_session")
private func rish_vm_boot_session(_ input: UnsafePointer<CChar>, _ count: Int) -> UnsafeMutableRawPointer?
@_silgen_name("rish_vm_session_exec_json")
private func rish_vm_session_exec_json(_ session: UnsafeMutableRawPointer, _ input: UnsafePointer<CChar>, _ count: Int) -> UnsafeMutablePointer<CChar>?
@_silgen_name("rish_vm_session_free")
private func rish_vm_session_free(_ session: UnsafeMutableRawPointer?)
@_silgen_name("rish_string_free")
private func rish_string_free(_ value: UnsafeMutablePointer<CChar>?)
@_silgen_name("rish_protocol_version")
private func rish_protocol_version() -> UInt32
#endif

/// A JSON value used by the Rust `serde_json::Value` fields.
///
/// Integer cases are kept separate so decoding and re-encoding a payload does
/// not silently round a Rust `u64` through `Double`.
public indirect enum RishJSONValue: Codable, Equatable, Sendable {
    case null
    case bool(Bool)
    case signed(Int64)
    case unsigned(UInt64)
    case number(Double)
    case string(String)
    case array([RishJSONValue])
    case object([String: RishJSONValue])

    public init(from decoder: Decoder) throws {
        let value = try decoder.singleValueContainer()
        if value.decodeNil() {
            self = .null
        } else if let decoded = try? value.decode(Bool.self) {
            self = .bool(decoded)
        } else if let decoded = try? value.decode(Int64.self) {
            self = .signed(decoded)
        } else if let decoded = try? value.decode(UInt64.self) {
            self = .unsigned(decoded)
        } else if let decoded = try? value.decode(Double.self) {
            self = .number(decoded)
        } else if let decoded = try? value.decode(String.self) {
            self = .string(decoded)
        } else if let decoded = try? value.decode([RishJSONValue].self) {
            self = .array(decoded)
        } else if let decoded = try? value.decode([String: RishJSONValue].self) {
            self = .object(decoded)
        } else {
            throw DecodingError.dataCorruptedError(
                in: value,
                debugDescription: "unsupported JSON value"
            )
        }
    }

    public func encode(to encoder: Encoder) throws {
        var value = encoder.singleValueContainer()
        switch self {
        case .null:
            try value.encodeNil()
        case let .bool(decoded):
            try value.encode(decoded)
        case let .signed(decoded):
            try value.encode(decoded)
        case let .unsigned(decoded):
            try value.encode(decoded)
        case let .number(decoded):
            try value.encode(decoded)
        case let .string(decoded):
            try value.encode(decoded)
        case let .array(decoded):
            try value.encode(decoded)
        case let .object(decoded):
            try value.encode(decoded)
        }
    }
}

/// JSON-compatible counterpart of `rish_core::GuestCommand`.
public struct RishGuestCommand: Codable, Equatable, Sendable {
    public var program: String
    public var args: [String]
    public var env: [String: String]
    public var cwd: String
    /// JSON encodes this as `[0, 255, ...]`, matching Rust `Vec<u8>`.
    public var stdin: [UInt8]

    private enum CodingKeys: String, CodingKey {
        case program, args, env, cwd, stdin
    }

    public init(
        program: String,
        args: [String] = [],
        env: [String: String] = [:],
        cwd: String = "/",
        stdin: [UInt8] = []
    ) {
        self.program = program
        self.args = args
        self.env = env
        self.cwd = cwd
        self.stdin = stdin
    }

    public init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        program = try values.decode(String.self, forKey: .program)
        args = try values.decodeIfPresent([String].self, forKey: .args) ?? []
        env = try values.decodeIfPresent([String: String].self, forKey: .env) ?? [:]
        cwd = try values.decodeIfPresent(String.self, forKey: .cwd) ?? "/"
        stdin = try values.decodeIfPresent([UInt8].self, forKey: .stdin) ?? []
    }
}

public struct RishAppletLimits: Codable, Equatable, Sendable {
    public var maxInputBytes: Int
    public var maxOutputBytes: Int
    public var maxFilesystemEntries: Int
    public var maxRecursionDepth: Int

    private enum CodingKeys: String, CodingKey {
        case maxInputBytes = "max_input_bytes"
        case maxOutputBytes = "max_output_bytes"
        case maxFilesystemEntries = "max_filesystem_entries"
        case maxRecursionDepth = "max_recursion_depth"
    }

    public init(
        maxInputBytes: Int = 1_048_576,
        maxOutputBytes: Int = 1_048_576,
        maxFilesystemEntries: Int = 10_000,
        maxRecursionDepth: Int = 64
    ) {
        self.maxInputBytes = maxInputBytes
        self.maxOutputBytes = maxOutputBytes
        self.maxFilesystemEntries = maxFilesystemEntries
        self.maxRecursionDepth = maxRecursionDepth
    }
}

public enum RishAppletConfigurationError: Error {
    case containerRootMustBeAbsolute
    case containerRootMustBeDirectory
    case sandboxRootMustBeDescendant
    case sandboxRootMustBeDirectory
    case unsafeIdentity
    case invalidLimits
}

/// Host-owned portable applet boundary.
///
/// `appContainerRoot` must come from an Apple container API, such as
/// `NSHomeDirectory()` or `FileManager.containerURL(...)`, never guest input.
public struct RishAppletConfiguration: Sendable {
    fileprivate let sandboxRoot: String
    fileprivate let readOnly: Bool
    fileprivate let user: String
    fileprivate let hostname: String
    fileprivate let limits: RishAppletLimits

    public init(
        sandboxRoot: URL,
        appContainerRoot: URL = URL(
            fileURLWithPath: NSHomeDirectory(),
            isDirectory: true
        ),
        readOnly: Bool = false,
        user: String = "rish",
        hostname: String = "rish",
        limits: RishAppletLimits = .init()
    ) throws {
        guard sandboxRoot.isFileURL,
              appContainerRoot.isFileURL,
              sandboxRoot.path.hasPrefix("/"),
              appContainerRoot.path.hasPrefix("/")
        else {
            throw RishAppletConfigurationError.containerRootMustBeAbsolute
        }
        guard Self.isSafeIdentity(user), Self.isSafeIdentity(hostname) else {
            throw RishAppletConfigurationError.unsafeIdentity
        }
        guard limits.maxInputBytes > 0,
              limits.maxInputBytes <= 1_048_576,
              limits.maxOutputBytes > 0,
              limits.maxOutputBytes <= 1_048_576,
              limits.maxFilesystemEntries > 0,
              limits.maxFilesystemEntries <= 100_000,
              limits.maxRecursionDepth > 0,
              limits.maxRecursionDepth <= 256
        else {
            throw RishAppletConfigurationError.invalidLimits
        }

        let fileManager = FileManager.default
        let lexicalContainer = appContainerRoot.standardizedFileURL
        let lexicalSandbox = sandboxRoot.standardizedFileURL
        guard lexicalContainer.path != "/",
              Self.isStrictDescendant(lexicalSandbox, of: lexicalContainer),
              lexicalSandbox.deletingLastPathComponent() == lexicalContainer,
              Self.isSafeIdentity(lexicalSandbox.lastPathComponent) else {
            throw RishAppletConfigurationError.sandboxRootMustBeDescendant
        }

        var containerIsDirectory: ObjCBool = false
        guard fileManager.fileExists(
            atPath: lexicalContainer.path,
            isDirectory: &containerIsDirectory
        ), containerIsDirectory.boolValue else {
            throw RishAppletConfigurationError.containerRootMustBeDirectory
        }
        let canonicalContainer = lexicalContainer.resolvingSymlinksInPath()
        let canonicalSandbox = canonicalContainer.appendingPathComponent(
            lexicalSandbox.lastPathComponent,
            isDirectory: true
        )
        if fileManager.fileExists(atPath: canonicalSandbox.path) {
            let values = try canonicalSandbox.resourceValues(
                forKeys: [.isDirectoryKey, .isSymbolicLinkKey]
            )
            guard values.isDirectory == true, values.isSymbolicLink != true else {
                throw RishAppletConfigurationError.sandboxRootMustBeDirectory
            }
        } else {
            try fileManager.createDirectory(
                at: canonicalSandbox,
                withIntermediateDirectories: false
            )
        }
        let verifiedSandbox = canonicalSandbox.resolvingSymlinksInPath()
        guard verifiedSandbox == canonicalSandbox,
              Self.isStrictDescendant(verifiedSandbox, of: canonicalContainer) else {
            throw RishAppletConfigurationError.sandboxRootMustBeDescendant
        }
        var sandboxIsDirectory: ObjCBool = false
        guard fileManager.fileExists(
            atPath: verifiedSandbox.path,
            isDirectory: &sandboxIsDirectory
        ), sandboxIsDirectory.boolValue else {
            throw RishAppletConfigurationError.sandboxRootMustBeDirectory
        }

        self.sandboxRoot = verifiedSandbox.path
        self.readOnly = readOnly
        self.user = user
        self.hostname = hostname
        self.limits = limits
    }

    private static func isStrictDescendant(_ child: URL, of parent: URL) -> Bool {
        let childComponents = child.pathComponents
        let parentComponents = parent.pathComponents
        return childComponents.count > parentComponents.count
            && childComponents.starts(with: parentComponents)
    }

    private static func isSafeIdentity(_ value: String) -> Bool {
        !value.isEmpty && value.utf8.count <= 64
            && value.unicodeScalars.allSatisfy { scalar in
                let character = scalar.value
                return (48 ... 57).contains(character)
                    || (65 ... 90).contains(character)
                    || (97 ... 122).contains(character)
                    || character == 45 || character == 46 || character == 95
            }
    }
}

/// JSON-compatible counterpart of `rish_core::CapabilityRequirement`.
public struct RishCapabilityRequirement: Codable, Equatable, Sendable {
    public var capability: String
    public var kernelSemanticsRequired: Bool

    private enum CodingKeys: String, CodingKey {
        case capability
        case kernelSemanticsRequired = "kernel_semantics_required"
    }

    public init(capability: String, kernelSemanticsRequired: Bool = false) {
        self.capability = capability
        self.kernelSemanticsRequired = kernelSemanticsRequired
    }

    public init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        capability = try values.decode(String.self, forKey: .capability)
        kernelSemanticsRequired =
            try values.decodeIfPresent(Bool.self, forKey: .kernelSemanticsRequired) ?? false
    }
}

/// JSON-compatible counterpart of `rish_core::HostCall`.
public struct RishHostCall: Codable, Equatable, Sendable {
    public var protocolVersion: UInt32
    public var id: UInt64
    public var operation: String
    public var command: RishGuestCommand
    public var requirements: [RishCapabilityRequirement]
    public var payload: RishJSONValue

    private enum CodingKeys: String, CodingKey {
        case protocolVersion = "protocol_version"
        case id, operation, command, requirements, payload
    }

    public init(
        protocolVersion: UInt32,
        id: UInt64,
        operation: String,
        command: RishGuestCommand,
        requirements: [RishCapabilityRequirement] = [],
        payload: RishJSONValue = .null
    ) {
        self.protocolVersion = protocolVersion
        self.id = id
        self.operation = operation
        self.command = command
        self.requirements = requirements
        self.payload = payload
    }

    public init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        protocolVersion = try values.decode(UInt32.self, forKey: .protocolVersion)
        id = try values.decode(UInt64.self, forKey: .id)
        operation = try values.decode(String.self, forKey: .operation)
        command = try values.decode(RishGuestCommand.self, forKey: .command)
        requirements =
            try values.decodeIfPresent(
                [RishCapabilityRequirement].self,
                forKey: .requirements
            ) ?? []
        payload = try values.decodeIfPresent(RishJSONValue.self, forKey: .payload) ?? .null
    }
}

/// JSON-compatible counterpart of `rish_core::HostReply`.
public struct RishHostReply: Codable, Equatable, Sendable {
    public var exitCode: Int32
    /// Byte arrays are deliberately not `Data`, whose Codable form is Base64.
    public var stdout: [UInt8]
    public var stderr: [UInt8]
    public var payload: RishJSONValue

    private enum CodingKeys: String, CodingKey {
        case exitCode = "exit_code"
        case stdout, stderr, payload
    }

    public init(
        exitCode: Int32,
        stdout: [UInt8] = [],
        stderr: [UInt8] = [],
        payload: RishJSONValue = .null
    ) {
        self.exitCode = exitCode
        self.stdout = stdout
        self.stderr = stderr
        self.payload = payload
    }

    static func failure(code: String, message: String, exitCode: Int32 = 125) -> Self {
        Self(
            exitCode: exitCode,
            stderr: Array("\(message)\n".utf8),
            payload: .object([
                "error": .object([
                    "code": .string(code),
                    "message": .string(message),
                ]),
            ])
        )
    }
}

/// An asynchronous, cancellation-aware native offload handler.
///
/// Long-running implementations should call `Task.checkCancellation()` at
/// suspension points. Output remains arbitrary bytes through `[UInt8]`.
public protocol RishHostOperationHandler: Sendable {
    func handle(_ call: RishHostCall) async throws -> RishHostReply
}

/// Adapter for an app-provided handler without making unknown operations
/// dynamically registerable.
public struct RishClosureHandler: RishHostOperationHandler {
    public typealias Body =
        @Sendable (RishHostCall) async throws -> RishHostReply

    private let body: Body

    public init(_ body: @escaping Body) {
        self.body = body
    }

    public func handle(_ call: RishHostCall) async throws -> RishHostReply {
        try await body(call)
    }
}

/// A tiny app-local service state machine.
///
/// This does not spawn PID 1, create cgroups, or implement real systemd units.
/// It only provides explicitly documented `systemctl`-like product semantics.
public actor RishInMemoryServiceSupervisor: RishHostOperationHandler {
    private static let maximumUnits = 256

    private enum State: String {
        case active
        case inactive
    }

    private var units: [String: State] = [:]

    public init() {}

    public func handle(_ call: RishHostCall) async throws -> RishHostReply {
        try Task.checkCancellation()
        guard call.operation == "service.systemctl",
              call.command.program.split(separator: "/").last == "systemctl"
        else {
            return .failure(
                code: "command_operation_mismatch",
                message: "service.systemctl only accepts the systemctl command"
            )
        }
        guard let verb = call.command.args.first else {
            return usage()
        }

        switch verb {
        case "list-units":
            guard call.command.args.count == 1 else {
                return usage()
            }
            let output = units.keys.sorted().map { unit in
                "\(unit) \(units[unit]?.rawValue ?? State.inactive.rawValue)"
            }.joined(separator: "\n")
            return reply(
                exitCode: 0,
                stdout: output.isEmpty ? "" : "\(output)\n",
                verb: verb,
                unit: nil
            )
        case "start", "stop", "restart", "status", "is-active":
            guard call.command.args.count == 2,
                  isSafeUnitName(call.command.args[1])
            else {
                return usage()
            }
            return apply(verb: verb, unit: call.command.args[1])
        default:
            return .failure(
                code: "unsupported_systemctl_verb",
                message: "in-memory supervisor does not implement that systemctl verb",
                exitCode: 2
            )
        }
    }

    private func apply(verb: String, unit: String) -> RishHostReply {
        switch verb {
        case "start":
            guard units[unit] != nil || units.count < Self.maximumUnits else {
                return capacityExceeded()
            }
            units[unit] = .active
            return reply(exitCode: 0, stdout: "", verb: verb, unit: unit)
        case "stop":
            if units[unit] != nil {
                units[unit] = .inactive
            }
            return reply(exitCode: 0, stdout: "", verb: verb, unit: unit)
        case "restart":
            guard units[unit] != nil || units.count < Self.maximumUnits else {
                return capacityExceeded()
            }
            units[unit] = .active
            return reply(exitCode: 0, stdout: "", verb: verb, unit: unit)
        case "status":
            let state = units[unit] ?? .inactive
            return reply(
                exitCode: state == .active ? 0 : 3,
                stdout: "\(unit): \(state.rawValue) (rish in-memory supervisor)\n",
                verb: verb,
                unit: unit
            )
        case "is-active":
            let state = units[unit] ?? .inactive
            return reply(
                exitCode: state == .active ? 0 : 3,
                stdout: "\(state.rawValue)\n",
                verb: verb,
                unit: unit
            )
        default:
            return .failure(code: "internal_dispatch_error", message: "invalid service verb")
        }
    }

    private func reply(
        exitCode: Int32,
        stdout: String,
        verb: String,
        unit: String?
    ) -> RishHostReply {
        var metadata: [String: RishJSONValue] = [
            "implementation": .string("rish.in_memory_supervisor"),
            "real_systemd": .bool(false),
            "verb": .string(verb),
        ]
        if let unit {
            metadata["unit"] = .string(unit)
        }
        return RishHostReply(
            exitCode: exitCode,
            stdout: Array(stdout.utf8),
            payload: .object(metadata)
        )
    }

    private func usage() -> RishHostReply {
        .failure(
            code: "invalid_systemctl_arguments",
            message: "usage: systemctl <start|stop|restart|status|is-active> <unit> | list-units",
            exitCode: 2
        )
    }

    private func capacityExceeded() -> RishHostReply {
        .failure(
            code: "supervisor_capacity_exceeded",
            message: "in-memory supervisor unit limit reached"
        )
    }

    private func isSafeUnitName(_ unit: String) -> Bool {
        guard !unit.isEmpty, unit.utf8.count <= 128 else {
            return false
        }
        return unit.unicodeScalars.allSatisfy { scalar in
            let value = scalar.value
            return (48 ... 57).contains(value)
                || (65 ... 90).contains(value)
                || (97 ... 122).contains(value)
                || value == 45 || value == 46 || value == 64 || value == 95
        }
    }
}

/// Fail-closed dispatcher for the portable native-offload surface.
///
/// The allow-list is intentionally a `switch`, not a mutable handler map.
/// Applications may inject only the named Docker API facade slot.
public final class RishHostDispatcher: Sendable {
    public static let protocolVersion: UInt32 = 1

    private let supervisor: RishInMemoryServiceSupervisor
    private let dockerAPIHandler: (any RishHostOperationHandler)?

    public init(
        supervisor: RishInMemoryServiceSupervisor = .init(),
        dockerAPIHandler: (any RishHostOperationHandler)? = nil
    ) {
        self.supervisor = supervisor
        self.dockerAPIHandler = dockerAPIHandler
    }

    public func dispatch(_ call: RishHostCall) async -> RishHostReply {
        guard call.protocolVersion == Self.protocolVersion else {
            return .failure(
                code: "unsupported_protocol_version",
                message: "host supports protocol version \(Self.protocolVersion)"
            )
        }
        guard Self.isWithinInputLimits(call) else {
            return .failure(
                code: "host_call_limits_exceeded",
                message: "host call exceeds portable dispatcher limits"
            )
        }
        guard !call.requirements.contains(where: \.kernelSemanticsRequired) else {
            return .failure(
                code: "kernel_semantics_unavailable",
                message: "portable iOS offload cannot satisfy real Linux kernel semantics"
            )
        }

        do {
            try Task.checkCancellation()
            let reply: RishHostReply
            switch call.operation {
            case "service.systemctl":
                reply = try await supervisor.handle(call)
            case "container.docker_api":
                guard call.command.program.split(separator: "/").last == "docker" else {
                    return .failure(
                        code: "command_operation_mismatch",
                        message: "container.docker_api only accepts the docker command"
                    )
                }
                guard let dockerAPIHandler else {
                    return .failure(
                        code: "docker_api_unavailable",
                        message: "Docker API handler is not configured; no dockerd was started"
                    )
                }
                reply = try await dockerAPIHandler.handle(call)
            default:
                return .failure(
                    code: "operation_not_allow_listed",
                    message: "native operation is not allow-listed",
                    exitCode: 126
                )
            }
            try Task.checkCancellation()
            guard reply.stdout.count <= 8 * 1_024 * 1_024,
                  reply.stderr.count <= 8 * 1_024 * 1_024
            else {
                return .failure(
                    code: "host_reply_limits_exceeded",
                    message: "native handler output exceeds dispatcher limits"
                )
            }
            return reply
        } catch is CancellationError {
            return .failure(code: "cancelled", message: "native operation was cancelled", exitCode: 130)
        } catch {
            return .failure(
                code: "handler_failed",
                message: "native handler failed: \(error.localizedDescription)"
            )
        }
    }

    /// The returned task is the cancellation handle for this operation.
    public func submit(_ call: RishHostCall) -> Task<RishHostReply, Never> {
        Task {
            await dispatch(call)
        }
    }

    private static func isWithinInputLimits(_ call: RishHostCall) -> Bool {
        call.operation.utf8.count <= 128
            && call.command.program.utf8.count <= 4_096
            && call.command.args.count <= 64
            && call.command.args.allSatisfy { $0.utf8.count <= 4_096 }
            && call.command.env.count <= 128
            && call.command.env.allSatisfy {
                $0.key.utf8.count <= 1_024 && $0.value.utf8.count <= 65_536
            }
            && call.command.stdin.count <= 1_048_576
            && call.requirements.count <= 64
            && call.requirements.allSatisfy { $0.capability.utf8.count <= 128 }
    }
}

/// Swift adapter around the versioned Rust JSON planning ABI and host-call
/// codec. Planning never dispatches a host operation by itself.
public enum RishBridge {
    private struct TypedPlanRequest: Encodable {
        let platform = "ios"
        let privilege = "app_sandbox"
        let command: RishGuestCommand
    }

    private struct TypedPlanResponse: Decodable {
        let protocolVersion: UInt32
        let ok: Bool
        let plan: TypedPlan?
        let error: String?

        private enum CodingKeys: String, CodingKey {
            case protocolVersion = "protocol_version"
            case ok, plan, error
        }
    }

    private struct TypedPlan: Decodable {
        let kind: String
        let name: String?
    }

    private struct ExecuteAppletRequest: Encodable {
        let protocolVersion: UInt32
        let sandboxRoot: String
        let readOnly: Bool
        let user: String
        let hostname: String
        let limits: RishAppletLimits
        let command: RishGuestCommand

        private enum CodingKeys: String, CodingKey {
            case protocolVersion = "protocol_version"
            case sandboxRoot = "sandbox_root"
            case readOnly = "read_only"
            case user, hostname, limits, command
        }
    }

    public static var protocolVersion: UInt32 {
        rish_protocol_version()
    }

    public static func plan(request: Data) throws -> Data {
        guard request.count <= 8 * 1_024 * 1_024 else {
            throw BridgeError.inputTooLarge
        }
        guard let request = String(data: request, encoding: .utf8) else {
            throw BridgeError.invalidUTF8
        }

        return try request.withCString { input in
            guard let rawResponse = rish_plan_json(input, request.utf8.count) else {
                throw BridgeError.nullResponse
            }
            defer { rish_string_free(rawResponse) }
            return Data(String(cString: rawResponse).utf8)
        }
    }

    /// Plans a typed iOS command and executes only a `portable_applet` plan.
    /// The sandbox root is copied exclusively from host configuration.
    public static func executePortableApplet(
        command: RishGuestCommand,
        configuration: RishAppletConfiguration
    ) throws -> Data {
        guard command.stdin.count <= configuration.limits.maxInputBytes else {
            throw BridgeError.inputTooLarge
        }
        let planRequest = try JSONEncoder().encode(TypedPlanRequest(command: command))
        let planResponseData = try plan(request: planRequest)
        let planned = try JSONDecoder().decode(
            TypedPlanResponse.self,
            from: planResponseData
        )
        guard planned.protocolVersion == protocolVersion, planned.ok else {
            throw BridgeError.plannerRejected(planned.error ?? "planner rejected command")
        }
        let name = command.program.split(separator: "/").last.map(String.init) ?? ""
        guard planned.plan?.kind == "portable_applet",
              planned.plan?.name == name else {
            throw BridgeError.notPortableApplet
        }

        let request = ExecuteAppletRequest(
            protocolVersion: protocolVersion,
            sandboxRoot: configuration.sandboxRoot,
            readOnly: configuration.readOnly,
            user: configuration.user,
            hostname: configuration.hostname,
            limits: configuration.limits,
            command: command
        )
        let encoded = try JSONEncoder().encode(request)
        guard encoded.count <= 8 * 1_024 * 1_024,
              let json = String(data: encoded, encoding: .utf8) else {
            throw BridgeError.inputTooLarge
        }
        return try json.withCString { input in
            guard let rawResponse = rish_execute_applet_json(input, json.utf8.count) else {
                throw BridgeError.nullResponse
            }
            defer { rish_string_free(rawResponse) }
            let response = Data(String(cString: rawResponse).utf8)
            guard response.count <= 8 * 1_024 * 1_024 else {
                throw BridgeError.inputTooLarge
            }
            return response
        }
    }

    public static func decodeHostCall(_ json: Data) throws -> RishHostCall {
        guard json.count <= 8 * 1_024 * 1_024 else {
            throw BridgeError.inputTooLarge
        }
        return try JSONDecoder().decode(RishHostCall.self, from: json)
    }

    public static func encodeHostReply(_ reply: RishHostReply) throws -> Data {
        try JSONEncoder().encode(reply)
    }

    public static func dispatchHostCall(
        _ json: Data,
        using dispatcher: RishHostDispatcher
    ) async throws -> Data {
        let call = try decodeHostCall(json)
        let reply = await dispatcher.dispatch(call)
        return try encodeHostReply(reply)
    }

    /// A full-VM docker request. `kernelPath` and `initrdPath` name guest
    /// binaries staged as app bundle resources; they never cross the ABI as
    /// data. `command` is the argv run inside the booted Linux guest.
    public struct RishVMRunRequest: Encodable, Sendable {
        public var kernelPath: String
        public var initrdPath: String
        public var rootDiskPath: String?
        public var memoryMib: UInt32
        public var command: [String]
        public var commandLine: String?
        public var bootBudgetUnits: UInt64?
        public var handshakeBudgetUnits: UInt64?

        private enum CodingKeys: String, CodingKey {
            case kernelPath = "kernel_path"
            case initrdPath = "initrd_path"
            case rootDiskPath = "root_disk_path"
            case memoryMib = "memory_mib"
            case command
            case commandLine = "command_line"
            case bootBudgetUnits = "boot_budget_units"
            case handshakeBudgetUnits = "handshake_budget_units"
        }

        public init(
            kernelPath: String,
            initrdPath: String,
            command: [String],
            rootDiskPath: String? = nil,
            memoryMib: UInt32 = 1024,
            commandLine: String? = nil,
            bootBudgetUnits: UInt64? = nil,
            handshakeBudgetUnits: UInt64? = nil
        ) {
            self.kernelPath = kernelPath
            self.initrdPath = initrdPath
            self.command = command
            self.rootDiskPath = rootDiskPath
            self.memoryMib = memoryMib
            self.commandLine = commandLine
            self.bootBudgetUnits = bootBudgetUnits
            self.handshakeBudgetUnits = handshakeBudgetUnits
        }
    }

    /// The decoded reply from `rish_vm_run_docker_json`.
    public struct RishVMRunResult: Decodable, Sendable {
        public let protocolVersion: UInt32
        public let ok: Bool
        public let exitCode: Int32?
        public let stdout: String?
        public let stderr: String?
        public let bootUnits: UInt64?
        public let error: String?

        private enum CodingKeys: String, CodingKey {
            case protocolVersion = "protocol_version"
            case ok
            case exitCode = "exit_code"
            case stdout, stderr
            case bootUnits = "boot_units"
            case error
        }
    }

    /// Boots the pure-Rust x86_64 interpreter and runs one command inside the
    /// guest — the full docker surface. This boots a Linux guest and blocks, so
    /// call it from a background thread.
    public static func runDockerVM(_ request: RishVMRunRequest) throws -> RishVMRunResult {
        let encoded = try JSONEncoder().encode(request)
        guard encoded.count <= 8 * 1_024 * 1_024,
              let json = String(data: encoded, encoding: .utf8) else {
            throw BridgeError.inputTooLarge
        }
        return try json.withCString { input in
            guard let rawResponse = rish_vm_run_docker_json(input, json.utf8.count) else {
                throw BridgeError.nullResponse
            }
            defer { rish_string_free(rawResponse) }
            let data = Data(String(cString: rawResponse).utf8)
            return try JSONDecoder().decode(RishVMRunResult.self, from: data)
        }
    }

    public enum BridgeError: Error {
        case invalidUTF8
        case nullResponse
        case inputTooLarge
        case plannerRejected(String)
        case notPortableApplet
    }
}

/// A live interactive guest session: boot the x86-64 Linux guest once, then run
/// many commands over the same open control channel. Every call blocks on the
/// interpreter, so drive it from a background thread. Releasing the object shuts
/// the guest down.
public final class RishVMSession {
    private let handle: UnsafeMutableRawPointer

    private init(handle: UnsafeMutableRawPointer) {
        self.handle = handle
    }

    /// Boots a guest and returns a session, or nil if the boot fails.
    public static func boot(_ request: RishBridge.RishVMRunRequest) -> RishVMSession? {
        guard let encoded = try? JSONEncoder().encode(request),
              let json = String(data: encoded, encoding: .utf8) else { return nil }
        return json.withCString { input in
            guard let handle = rish_vm_boot_session(input, json.utf8.count) else { return nil }
            return RishVMSession(handle: handle)
        }
    }

    /// Runs one command in the live guest and returns the decoded result.
    public func run(_ command: [String]) throws -> RishBridge.RishVMRunResult {
        struct ExecRequest: Encodable { let command: [String] }
        let encoded = try JSONEncoder().encode(ExecRequest(command: command))
        guard let json = String(data: encoded, encoding: .utf8) else {
            throw RishBridge.BridgeError.invalidUTF8
        }
        return try json.withCString { input in
            guard let raw = rish_vm_session_exec_json(handle, input, json.utf8.count) else {
                throw RishBridge.BridgeError.nullResponse
            }
            defer { rish_string_free(raw) }
            let data = Data(String(cString: raw).utf8)
            return try JSONDecoder().decode(RishBridge.RishVMRunResult.self, from: data)
        }
    }

    deinit {
        rish_vm_session_free(handle)
    }
}
