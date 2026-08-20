import Foundation

enum RishDemoPullPhase: Equatable, Sendable {
    case normalizing
    case authenticating
    case resolving
    case selectingPlatform
    case fetchingManifest
    case fetchingConfig
    case downloading
    case verifying
    case storing

    var title: String {
        switch self {
        case .normalizing:
            return "Normalizing reference"
        case .authenticating:
            return "Authenticating"
        case .resolving:
            return "Resolving manifest"
        case .selectingPlatform:
            return "Selecting guest platform"
        case .fetchingManifest:
            return "Fetching manifest"
        case .fetchingConfig:
            return "Fetching image config"
        case .downloading:
            return "Downloading blobs"
        case .verifying:
            return "Verifying SHA-256"
        case .storing:
            return "Committing to CAS"
        }
    }
}

enum RishDemoPullItemState: Equatable, Sendable {
    case pending
    case downloading
    case verifying
    case cached
    case stored
    case failed

    var title: String {
        switch self {
        case .pending:
            return "PENDING"
        case .downloading:
            return "DOWNLOADING"
        case .verifying:
            return "VERIFYING"
        case .cached:
            return "CACHED"
        case .stored:
            return "VERIFIED"
        case .failed:
            return "FAILED"
        }
    }
}

struct RishDemoPullItem: Equatable, Identifiable, Sendable {
    let id: String
    let kind: String
    let digest: String?
    let receivedBytes: UInt64
    let totalBytes: UInt64?
    let state: RishDemoPullItemState
}

struct RishDemoPullProgress: Equatable, Sendable {
    let phase: RishDemoPullPhase
    let detail: String
    let receivedBytes: UInt64
    let totalBytes: UInt64?
    let verifiedBytes: UInt64
    let items: [RishDemoPullItem]
}

struct RishDemoPullReceipt: Equatable, Sendable {
    let canonicalReference: String
    let resolvedDigest: String
    let manifestDigest: String?
    let operatingSystem: String
    let architecture: String
    let variant: String?
    let layerCount: Int
    let downloadedBytes: UInt64
    let verifiedBytes: UInt64
    let casPin: String
}

struct RishDemoPullFailure: Error, Equatable, Sendable {
    enum Kind: Equatable, Sendable {
        case cancelled
        case invalidReference
        case network
        case registry
        case verification
        case storage
        case unknown
    }

    let kind: Kind
    let code: String
    let message: String
    let retryable: Bool
}

protocol RishDemoImagePulling: AnyObject, Sendable {
    func pull(
        reference: String,
        platform: RishImageGuestPlatform,
        progress: @escaping @Sendable (RishDemoPullProgress) -> Void,
        completion: @escaping @Sendable (
            Result<RishDemoPullReceipt, RishDemoPullFailure>
        ) -> Void
    )

    func cancel()
}

enum RishDemoAutoPull {
    static func requestedReference(
        arguments: [String] = ProcessInfo.processInfo.arguments
    ) -> String? {
        guard let flag = arguments.firstIndex(of: "--rish-auto-pull") else {
            return nil
        }
        let valueIndex = arguments.index(after: flag)
        guard arguments.indices.contains(valueIndex) else {
            return nil
        }
        let reference = arguments[valueIndex]
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard !reference.isEmpty,
              reference.utf8.count <= 512,
              reference.unicodeScalars.allSatisfy({
                  let value = $0.value
                  return value >= 32
                      && value != 127
                      && !(128 ... 159).contains(value)
              }) else {
            return nil
        }
        return reference
    }

    static func requestedPlatform(
        arguments: [String] = ProcessInfo.processInfo.arguments
    ) -> RishImageGuestPlatform? {
        guard let flag = arguments.firstIndex(of: "--rish-auto-platform") else {
            return .linuxArm64V8
        }
        let valueIndex = arguments.index(after: flag)
        guard arguments.indices.contains(valueIndex) else {
            return nil
        }
        return RishImageGuestPlatform(rawValue: arguments[valueIndex])
    }
}

enum RishDemoPullResultStore {
    private struct Platform: Encodable {
        let os: String
        let architecture: String
        let variant: String?
    }

    private struct Document: Encodable {
        let protocolVersion: UInt32
        let ok: Bool
        let reference: String
        let resolvedDigest: String?
        let platform: Platform?
        let layers: Int?
        let verifiedBytes: UInt64?
        let casPin: String?
        let error: String?

        private enum CodingKeys: String, CodingKey {
            case protocolVersion = "protocol_version"
            case ok, reference
            case resolvedDigest = "resolved_digest"
            case platform, layers
            case verifiedBytes = "verified_bytes"
            case casPin = "cas_pin"
            case error
        }
    }

    static func clear() {
        guard let url = resultURL() else {
            return
        }
        try? FileManager.default.removeItem(at: url)
    }

    static func persistSuccess(_ receipt: RishDemoPullReceipt) {
        persist(
            Document(
                protocolVersion: RishBridge.protocolVersion,
                ok: true,
                reference: receipt.canonicalReference,
                resolvedDigest: receipt.resolvedDigest,
                platform: Platform(
                    os: receipt.operatingSystem,
                    architecture: receipt.architecture,
                    variant: receipt.variant
                ),
                layers: receipt.layerCount,
                verifiedBytes: receipt.verifiedBytes,
                casPin: receipt.casPin,
                error: nil
            )
        )
    }

    static func persistFailure(
        reference: String,
        failure: RishDemoPullFailure
    ) {
        persist(
            Document(
                protocolVersion: RishBridge.protocolVersion,
                ok: false,
                reference: reference,
                resolvedDigest: nil,
                platform: nil,
                layers: nil,
                verifiedBytes: nil,
                casPin: nil,
                error: "\(failure.code): \(failure.message)"
            )
        )
    }

    private static func persist(_ document: Document) {
        do {
            let data = try JSONEncoder().encode(document)
            guard let url = resultURL() else {
                return
            }
            try data.write(to: url, options: .atomic)
        } catch {
            print("RISH_PULL_RESULT persist_error=\(error)")
        }
    }

    private static func resultURL() -> URL? {
        FileManager.default.urls(
            for: .documentDirectory,
            in: .userDomainMask
        ).first?.appendingPathComponent("RishPullResult.json")
    }
}
