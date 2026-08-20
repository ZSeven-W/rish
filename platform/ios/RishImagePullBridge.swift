import Darwin
import Foundation

private typealias RishRegistryFetchFunction = @convention(c) (
    UnsafeMutableRawPointer?,
    UnsafePointer<UInt8>?,
    Int,
    Int32,
    UnsafeMutablePointer<UInt8>?,
    Int,
    UnsafeMutablePointer<Int>?
) -> Int32

#if RISH_STANDALONE_TYPECHECK
@_silgen_name("rish_pull_image_json")
private func rish_pull_image_json(
    _ input: UnsafePointer<CChar>?,
    _ inputLength: Int,
    _ fetch: RishRegistryFetchFunction?,
    _ context: UnsafeMutableRawPointer?
) -> UnsafeMutablePointer<CChar>?

@_silgen_name("rish_string_free")
private func rish_string_free(_ value: UnsafeMutablePointer<CChar>?)
#endif

public enum RishImagePullPhase: String, Sendable {
    case connecting
    case authenticating
    case downloadingManifest = "downloading_manifest"
    case downloadingBlob = "downloading_blob"
    case verifying
    case complete
    case cancelled
}

public enum RishImageGuestPlatform: String, Codable, CaseIterable, Sendable {
    case linuxArm64V8 = "linux/arm64/v8"
    case linuxAmd64 = "linux/amd64"

    public var operatingSystem: String { "linux" }

    public var architecture: String {
        switch self {
        case .linuxArm64V8:
            return "arm64"
        case .linuxAmd64:
            return "amd64"
        }
    }

    public var variant: String? {
        switch self {
        case .linuxArm64V8:
            return "v8"
        case .linuxAmd64:
            return nil
        }
    }

    fileprivate func accepts(_ receipt: RishImagePullReceipt) -> Bool {
        guard receipt.os == operatingSystem,
              receipt.architecture == architecture else {
            return false
        }
        switch self {
        case .linuxArm64V8:
            return receipt.variant == nil || receipt.variant == "v8"
        case .linuxAmd64:
            return receipt.variant == nil
        }
    }
}

public struct RishImagePullProgress: Sendable {
    public let phase: RishImagePullPhase
    public let path: String
    public let digest: String?
    public let receivedBytes: UInt64
    public let expectedBytes: UInt64?
    public let aggregateReceivedBytes: UInt64
}

public struct RishImagePullLayerReceipt: Codable, Equatable, Sendable {
    public let digest: String
    public let size: UInt64
    public let mediaType: String

    private enum CodingKeys: String, CodingKey {
        case digest, size
        case mediaType = "media_type"
    }
}

public struct RishImagePullReceipt: Codable, Equatable, Sendable {
    public let normalizedReference: String
    public let resolvedDigest: String
    public let indexDigest: String?
    public let manifestDigest: String
    public let configDigest: String
    public let os: String
    public let architecture: String
    public let variant: String?
    public let layers: [RishImagePullLayerReceipt]
    public let totalVerifiedBytes: UInt64
    public let pin: String
    public let contentStore: String

    private enum CodingKeys: String, CodingKey {
        case normalizedReference = "normalized_reference"
        case resolvedDigest = "resolved_digest"
        case indexDigest = "index_digest"
        case manifestDigest = "manifest_digest"
        case configDigest = "config_digest"
        case os, architecture, variant, layers
        case totalVerifiedBytes = "total_verified_bytes"
        case pin
        case contentStore = "content_store"
    }

    public var digest: String? { resolvedDigest }
    public var reference: String? { normalizedReference }
    public var layerCount: UInt64? { UInt64(layers.count) }
    public var storedBytes: UInt64? { totalVerifiedBytes }
    public var operatingSystem: String? { os }
}

public struct RishImagePullResult: Codable, Equatable, Sendable {
    public let protocolVersion: UInt32
    public let ok: Bool
    public let receipt: RishImagePullReceipt?
    public let error: String?

    private enum CodingKeys: String, CodingKey {
        case protocolVersion = "protocol_version"
        case ok, receipt, error
    }
}

public enum RishImagePullClientError: Error, Equatable, Sendable {
    case invalidStoreRoot
    case requestEncoding(String)
    case nullResponse
    case invalidResponse(String)
    case rejected(String)
    case cancelled
}

public final class RishImagePullTask: @unchecked Sendable {
    private let lock = NSLock()
    private var cancellation: (() -> Void)?

    fileprivate init(_ cancellation: @escaping () -> Void) {
        self.cancellation = cancellation
    }

    public func cancel() {
        lock.lock()
        let cancellation = cancellation
        self.cancellation = nil
        lock.unlock()
        cancellation?()
    }

}

public final class RishImagePullClient: @unchecked Sendable {
    private let lock = NSLock()
    private let workQueue = DispatchQueue(label: "dev.rish.image-pull", qos: .userInitiated)
    private let callbackQueue: DispatchQueue
    private var generation: UInt64 = 0
    private var current: PullContext?

    public init(callbackQueue: DispatchQueue = .main) {
        self.callbackQueue = callbackQueue
    }

    @discardableResult
    public func pull(
        reference: String,
        platform: RishImageGuestPlatform = .linuxArm64V8,
        storeRoot: URL,
        progress: @escaping @Sendable (RishImagePullProgress) -> Void,
        completion: @escaping @Sendable (
            Result<RishImagePullResult, RishImagePullClientError>
        ) -> Void
    ) -> RishImagePullTask {
        let context: PullContext
        lock.lock()
        generation &+= 1
        let nextGeneration = generation
        current?.cancel()
        context = PullContext { [weak self] update in
            self?.deliver(update, generation: nextGeneration, to: progress)
        }
        current = context
        lock.unlock()

        workQueue.async { [weak self] in
            self?.perform(
                reference: reference,
                platform: platform,
                storeRoot: storeRoot,
                context: context,
                generation: nextGeneration,
                completion: completion
            )
        }
        return RishImagePullTask { [weak self] in
            self?.cancel(generation: nextGeneration)
        }
    }

    public func cancel() {
        lock.lock()
        let current = current
        lock.unlock()
        current?.cancel()
    }

    deinit {
        cancel()
    }

    private func cancel(generation expected: UInt64) {
        lock.lock()
        guard generation == expected else {
            lock.unlock()
            return
        }
        let current = current
        lock.unlock()
        current?.cancel()
    }

    private func perform(
        reference: String,
        platform: RishImageGuestPlatform,
        storeRoot: URL,
        context: PullContext,
        generation: UInt64,
        completion: @escaping @Sendable (
            Result<RishImagePullResult, RishImagePullClientError>
        ) -> Void
    ) {
        let result: Result<RishImagePullResult, RishImagePullClientError>
        do {
            guard let normalizedStoreRoot = Self.safeStoreRoot(storeRoot) else {
                throw RishImagePullClientError.invalidStoreRoot
            }
            let request = PullRequest(
                reference: reference,
                storeRoot: normalizedStoreRoot.path,
                platform: platform
            )
            let encoded: Data
            do {
                encoded = try JSONEncoder().encode(request)
            } catch {
                throw RishImagePullClientError.requestEncoding(error.localizedDescription)
            }
            guard encoded.count <= 64 * 1_024,
                  let json = String(data: encoded, encoding: .utf8)
            else {
                throw RishImagePullClientError.requestEncoding("request is too large")
            }
            if context.isCancelled {
                throw RishImagePullClientError.cancelled
            }

            let opaque = Unmanaged.passUnretained(context).toOpaque()
            let raw = json.withCString {
                rish_pull_image_json($0, json.utf8.count, registryFetchCallback, opaque)
            }
            if context.isCancelled {
                if let raw {
                    rish_string_free(raw)
                }
                throw RishImagePullClientError.cancelled
            }
            guard let raw else {
                throw RishImagePullClientError.nullResponse
            }
            defer { rish_string_free(raw) }
            let response = Data(String(cString: raw).utf8)
            guard response.count <= 8 * 1_024 * 1_024 else {
                throw RishImagePullClientError.invalidResponse("response is too large")
            }
            let decoded: RishImagePullResult
            do {
                decoded = try JSONDecoder().decode(RishImagePullResult.self, from: response)
            } catch {
                throw RishImagePullClientError.invalidResponse(error.localizedDescription)
            }
            guard decoded.protocolVersion == 1 else {
                throw RishImagePullClientError.invalidResponse("unsupported protocol version")
            }
            guard decoded.ok else {
                throw RishImagePullClientError.rejected(decoded.error ?? "pull rejected")
            }
            guard let receipt = decoded.receipt,
                  platform.accepts(receipt) else {
                throw RishImagePullClientError.invalidResponse(
                    "verified receipt does not match the requested guest platform"
                )
            }
            context.emit(phase: .complete, path: "", received: 0, expected: nil)
            result = .success(decoded)
        } catch let error as RishImagePullClientError {
            result = .failure(error)
        } catch {
            result = .failure(.invalidResponse(error.localizedDescription))
        }
        finish(result, generation: generation, context: context, completion: completion)
    }

    private func finish(
        _ result: Result<RishImagePullResult, RishImagePullClientError>,
        generation expected: UInt64,
        context: PullContext,
        completion: @escaping @Sendable (
            Result<RishImagePullResult, RishImagePullClientError>
        ) -> Void
    ) {
        lock.lock()
        if generation == expected {
            current = nil
        }
        lock.unlock()
        if case .failure(.cancelled) = result {
            context.emit(phase: .cancelled, path: "", received: 0, expected: nil)
        }
        callbackQueue.async {
            completion(result)
        }
    }

    private func deliver(
        _ update: RishImagePullProgress,
        generation expected: UInt64,
        to progress: @escaping @Sendable (RishImagePullProgress) -> Void
    ) {
        lock.lock()
        let live = generation == expected
        lock.unlock()
        guard live else { return }
        callbackQueue.async { [weak self] in
            guard let self else { return }
            self.lock.lock()
            let stillLive = self.generation == expected
            self.lock.unlock()
            if stillLive {
                progress(update)
            }
        }
    }

    private static func safeStoreRoot(_ url: URL) -> URL? {
        guard url.isFileURL, url.path.hasPrefix("/") else { return nil }
        let root = url
            .resolvingSymlinksInPath()
            .standardizedFileURL
        let container = URL(
            fileURLWithPath: NSHomeDirectory(),
            isDirectory: true
        )
        .resolvingSymlinksInPath()
        .standardizedFileURL
        guard root.path.hasPrefix(container.path + "/") else { return nil }
        return root
    }

    private struct PullRequest: Encodable {
        let protocolVersion: UInt32 = 1
        let reference: String
        let storeRoot: String
        let platform: RishImageGuestPlatform

        private enum CodingKeys: String, CodingKey {
            case protocolVersion = "protocol_version"
            case reference
            case storeRoot = "store_root"
            case platform
        }
    }
}

private struct RegistryRequestEnvelope: Decodable {
    let protocolVersion: UInt32
    let request: RegistryRequest

    private enum CodingKeys: String, CodingKey {
        case protocolVersion = "protocol_version"
        case request
    }
}

struct RegistryRequest: Decodable {
    let method: String
    let scheme: String
    let authority: String
    let pathAndQuery: String
    let headers: [String: [String]]
    let body: [UInt8]
    let maxResponseBytes: UInt64

    private enum CodingKeys: String, CodingKey {
        case method, scheme, authority, headers, body
        case pathAndQuery = "path_and_query"
        case maxResponseBytes = "max_response_bytes"
    }
}

private struct RegistryMetadata: Encodable {
    let protocolVersion: UInt32 = 1
    let ok: Bool
    let status: Int?
    let headers: [String: [String]]
    let error: String?
    let retryable: Bool

    private enum CodingKeys: String, CodingKey {
        case protocolVersion = "protocol_version"
        case ok, status, headers, error, retryable
    }
}

final class PullContext: @unchecked Sendable {
    private let lock = NSLock()
    private let progress: @Sendable (RishImagePullProgress) -> Void
    private var sessions: [ObjectIdentifier: URLSession] = [:]
    private var cancelled = false
    private var aggregate: UInt64 = 0

    init(progress: @escaping @Sendable (RishImagePullProgress) -> Void) {
        self.progress = progress
    }

    var isCancelled: Bool {
        lock.lock()
        defer { lock.unlock() }
        return cancelled
    }

    func register(_ session: URLSession) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard !cancelled else { return false }
        sessions[ObjectIdentifier(session)] = session
        return true
    }

    func unregister(_ session: URLSession) {
        lock.lock()
        sessions.removeValue(forKey: ObjectIdentifier(session))
        lock.unlock()
    }

    func cancel() {
        lock.lock()
        guard !cancelled else {
            lock.unlock()
            return
        }
        cancelled = true
        let sessions = Array(sessions.values)
        self.sessions.removeAll()
        lock.unlock()
        sessions.forEach { $0.invalidateAndCancel() }
    }

    func emit(
        phase: RishImagePullPhase,
        path: String,
        received: UInt64,
        expected: UInt64?,
        aggregateDelta: UInt64 = 0
    ) {
        lock.lock()
        if aggregateDelta > 0 {
            let addition = aggregate.addingReportingOverflow(aggregateDelta)
            aggregate = addition.overflow ? UInt64.max : addition.partialValue
        }
        let total = aggregate
        let cancelled = cancelled
        lock.unlock()
        guard !cancelled || phase == .cancelled else { return }
        progress(
            RishImagePullProgress(
                phase: phase,
                path: path,
                digest: Self.digest(path),
                receivedBytes: received,
                expectedBytes: expected,
                aggregateReceivedBytes: total
            )
        )
    }

    private static func digest(_ path: String) -> String? {
        guard let range = path.range(of: "/blobs/") else { return nil }
        let value = String(path[range.upperBound...]).split(separator: "?").first.map(String.init)
        return value?.isEmpty == false ? value : nil
    }
}

private let registryFetchCallback: RishRegistryFetchFunction = {
    context,
    requestBytes,
    requestLength,
    bodyFD,
    metadataBytes,
    metadataCapacity,
    metadataLength in
    guard let context,
          let requestBytes,
          requestLength > 0,
          requestLength <= 1_048_576,
          let metadataLength
    else {
        return -1
    }
    let pull = Unmanaged<PullContext>.fromOpaque(context).takeUnretainedValue()
    let metadata: RegistryMetadata
    do {
        let envelope = try JSONDecoder().decode(
            RegistryRequestEnvelope.self,
            from: Data(bytes: requestBytes, count: requestLength)
        )
        guard envelope.protocolVersion == 1 else {
            throw RegistryFetchError(message: "unsupported registry protocol version", retryable: false)
        }
        let response = try RegistryFetcher(context: pull).fetch(envelope.request, bodyFD: bodyFD)
        metadata = RegistryMetadata(
            ok: true,
            status: response.status,
            headers: response.headers,
            error: nil,
            retryable: false
        )
    } catch let error as RegistryFetchError {
        metadata = RegistryMetadata(
            ok: false,
            status: nil,
            headers: [:],
            error: error.message,
            retryable: error.retryable
        )
    } catch {
        metadata = RegistryMetadata(
            ok: false,
            status: nil,
            headers: [:],
            error: "registry transport failed",
            retryable: false
        )
    }

    guard let encoded = try? JSONEncoder().encode(metadata) else { return -1 }
    metadataLength.pointee = encoded.count
    guard encoded.count <= metadataCapacity, let metadataBytes else { return -2 }
    encoded.copyBytes(to: metadataBytes, count: encoded.count)
    return 0
}

struct RegistryFetchError: Error {
    let message: String
    let retryable: Bool

    static let cancelled = Self(message: "pull cancelled", retryable: false)
}

struct RegistryHTTPResponse {
    let status: Int
    let headers: [String: [String]]
    let body: Data?
}
