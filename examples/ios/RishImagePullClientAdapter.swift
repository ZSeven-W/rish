import Foundation

final class RishImagePullClientAdapter:
    RishDemoImagePulling,
    @unchecked Sendable
{
    private let client = RishImagePullClient(callbackQueue: .main)
    private var task: RishImagePullTask?
    private var items: [String: RishDemoPullItem] = [:]
    private var itemOrder: [String] = []
    private var aggregateReceived: UInt64 = 0

    func pull(
        reference: String,
        platform: RishImageGuestPlatform,
        progress: @escaping @Sendable (RishDemoPullProgress) -> Void,
        completion: @escaping @Sendable (
            Result<RishDemoPullReceipt, RishDemoPullFailure>
        ) -> Void
    ) {
        let storeRoot = URL(
            fileURLWithPath: NSHomeDirectory(),
            isDirectory: true
        ).appendingPathComponent("rish-oci-store", isDirectory: true)

        do {
            try FileManager.default.createDirectory(
                at: storeRoot,
                withIntermediateDirectories: true
            )
        } catch {
            completion(
                .failure(
                    RishDemoPullFailure(
                        kind: .storage,
                        code: "store_unavailable",
                        message: "Could not open the app-owned OCI content store.",
                        retryable: true
                    )
                )
            )
            return
        }

        items.removeAll(keepingCapacity: true)
        itemOrder.removeAll(keepingCapacity: true)
        aggregateReceived = 0

        task = client.pull(
            reference: reference,
            platform: platform,
            storeRoot: storeRoot,
            progress: { [weak self] update in
                guard let self else {
                    return
                }
                progress(self.map(update))
            },
            completion: { [weak self] result in
                guard let self else {
                    return
                }
                if case let .success(response) = result,
                   let receipt = response.receipt {
                    self.reconcile(receipt)
                    progress(
                        RishDemoPullProgress(
                            phase: .storing,
                            detail: "The verified image graph is pinned in the content store.",
                            receivedBytes: self.aggregateReceived,
                            totalBytes: self.aggregateReceived,
                            verifiedBytes: receipt.totalVerifiedBytes,
                            items: self.itemOrder.compactMap {
                                self.items[$0]
                            }
                        )
                    )
                }
                self.task = nil
                completion(self.map(result, expectedPlatform: platform))
            }
        )
    }

    func cancel() {
        task?.cancel()
    }

    private func map(
        _ update: RishImagePullProgress
    ) -> RishDemoPullProgress {
        aggregateReceived = update.aggregateReceivedBytes
        if update.phase == .complete {
            markItemsStored()
        } else {
            updateItem(update)
        }
        return RishDemoPullProgress(
            phase: phase(update.phase),
            detail: detail(update),
            receivedBytes: aggregateReceived,
            totalBytes: nil,
            verifiedBytes: 0,
            items: itemOrder.compactMap { items[$0] }
        )
    }

    private func updateItem(_ update: RishImagePullProgress) {
        guard !update.path.isEmpty,
              update.phase == .downloadingManifest
                || update.phase == .downloadingBlob
                || update.phase == .verifying else {
            return
        }
        let key = update.path
        if items[key] == nil {
            itemOrder.append(key)
        }
        let previous = items[key]
        let received = update.phase == .verifying
            ? (previous?.receivedBytes ?? update.receivedBytes)
            : update.receivedBytes
        items[key] = RishDemoPullItem(
            id: key,
            kind: update.phase == .downloadingManifest
                ? "manifest"
                : kind(for: update.path),
            digest: update.digest ?? previous?.digest,
            receivedBytes: received,
            totalBytes: update.expectedBytes ?? previous?.totalBytes,
            state: update.phase == .verifying ? .verifying : .downloading
        )
    }

    private func markItemsStored() {
        for key in itemOrder {
            guard let item = items[key] else {
                continue
            }
            items[key] = RishDemoPullItem(
                id: item.id,
                kind: item.kind,
                digest: item.digest,
                receivedBytes: item.receivedBytes,
                totalBytes: item.totalBytes,
                state: .stored
            )
        }
    }

    private func reconcile(_ receipt: RishImagePullReceipt) {
        var remainingManifestDigests: [String] = []
        if let indexDigest = receipt.indexDigest {
            remainingManifestDigests.append(indexDigest)
        }
        for digest in [receipt.resolvedDigest, receipt.manifestDigest]
        where !remainingManifestDigests.contains(digest) {
            remainingManifestDigests.append(digest)
        }
        for key in itemOrder {
            guard let item = items[key],
                  item.kind == "manifest",
                  let digest = item.digest else {
                continue
            }
            remainingManifestDigests.removeAll { $0 == digest }
        }

        for key in itemOrder {
            guard let item = items[key] else {
                continue
            }
            let digest: String?
            if let existing = item.digest {
                digest = existing
            } else if item.kind == "manifest",
                      !remainingManifestDigests.isEmpty {
                digest = remainingManifestDigests.removeFirst()
            } else {
                digest = nil
            }
            items[key] = RishDemoPullItem(
                id: item.id,
                kind: item.kind,
                digest: digest,
                receivedBytes: item.receivedBytes,
                totalBytes: item.totalBytes,
                state: .stored
            )
        }
    }

    private func map(
        _ result: Result<RishImagePullResult, RishImagePullClientError>,
        expectedPlatform: RishImageGuestPlatform
    ) -> Result<RishDemoPullReceipt, RishDemoPullFailure> {
        switch result {
        case let .success(response):
            guard response.ok, let receipt = response.receipt else {
                return .failure(
                    failure(
                        kind: .registry,
                        code: "pull_rejected",
                        message: response.error ?? "Registry pull was rejected.",
                        retryable: false
                    )
                )
            }
            return map(receipt, expectedPlatform: expectedPlatform)
        case let .failure(error):
            return .failure(map(error))
        }
    }

    private func map(
        _ receipt: RishImagePullReceipt,
        expectedPlatform: RishImageGuestPlatform
    ) -> Result<RishDemoPullReceipt, RishDemoPullFailure> {
        let variantMatches: Bool
        switch expectedPlatform {
        case .linuxArm64V8:
            variantMatches = receipt.variant == nil || receipt.variant == "v8"
        case .linuxAmd64:
            variantMatches = receipt.variant == nil
        }
        guard !receipt.normalizedReference.isEmpty,
              !receipt.resolvedDigest.isEmpty,
              !receipt.manifestDigest.isEmpty,
              !receipt.pin.isEmpty,
              receipt.os == expectedPlatform.operatingSystem,
              receipt.architecture == expectedPlatform.architecture,
              variantMatches else {
            return .failure(
                failure(
                    kind: .verification,
                    code: "invalid_verified_receipt",
                    message: "The pull completed without a receipt for the selected guest platform.",
                    retryable: false
                )
            )
        }

        return .success(
            RishDemoPullReceipt(
                canonicalReference: receipt.normalizedReference,
                resolvedDigest: receipt.resolvedDigest,
                manifestDigest: receipt.manifestDigest,
                operatingSystem: receipt.os,
                architecture: receipt.architecture,
                variant: receipt.variant,
                layerCount: receipt.layers.count,
                downloadedBytes: aggregateReceived,
                verifiedBytes: receipt.totalVerifiedBytes,
                casPin: receipt.pin
            )
        )
    }

    private func map(
        _ error: RishImagePullClientError
    ) -> RishDemoPullFailure {
        switch error {
        case .cancelled:
            return failure(
                kind: .cancelled,
                code: "cancelled",
                message: "The pull was cancelled.",
                retryable: true
            )
        case .invalidStoreRoot:
            return failure(
                kind: .storage,
                code: "invalid_store_root",
                message: "The OCI store is outside the app container.",
                retryable: false
            )
        case let .requestEncoding(message):
            return failure(
                kind: .invalidReference,
                code: "invalid_pull_request",
                message: message,
                retryable: false
            )
        case .nullResponse:
            return failure(
                kind: .unknown,
                code: "empty_runtime_response",
                message: "The Rust pull runtime returned no response.",
                retryable: true
            )
        case let .invalidResponse(message):
            return failure(
                kind: .registry,
                code: "invalid_runtime_response",
                message: message,
                retryable: false
            )
        case let .rejected(message):
            let lowered = message.lowercased()
            let kind: RishDemoPullFailure.Kind
            let retryable: Bool
            if lowered.contains("digest")
                || lowered.contains("verify")
                || lowered.contains("sha-256") {
                kind = .verification
                retryable = false
            } else if lowered.contains("store")
                || lowered.contains("space")
                || lowered.contains("quota") {
                kind = .storage
                retryable = false
            } else if lowered.contains("network")
                || lowered.contains("transport")
                || lowered.contains("timed out") {
                kind = .network
                retryable = true
            } else {
                kind = .registry
                retryable = false
            }
            return failure(
                kind: kind,
                code: "pull_rejected",
                message: message,
                retryable: retryable
            )
        }
    }

    private func phase(_ phase: RishImagePullPhase) -> RishDemoPullPhase {
        switch phase {
        case .connecting:
            return .resolving
        case .authenticating:
            return .authenticating
        case .downloadingManifest:
            return .fetchingManifest
        case .downloadingBlob:
            return .downloading
        case .verifying:
            return .verifying
        case .complete:
            return .storing
        case .cancelled:
            return .storing
        }
    }

    private func detail(_ update: RishImagePullProgress) -> String {
        switch update.phase {
        case .connecting:
            return "Connecting over HTTPS with bounded response limits…"
        case .authenticating:
            return "Requesting an anonymous registry bearer token…"
        case .downloadingManifest:
            return "Streaming an OCI manifest or image index…"
        case .downloadingBlob:
            return update.digest.map {
                "Streaming \($0) into the content pipeline…"
            } ?? "Streaming a bounded image blob…"
        case .verifying:
            return update.digest.map {
                "Checking size and SHA-256 for \($0)…"
            } ?? "Checking response size and SHA-256…"
        case .complete:
            return "Publishing the verified image graph to the CAS…"
        case .cancelled:
            return "The registry session acknowledged cancellation."
        }
    }

    private func kind(for path: String) -> String {
        path.contains("/manifests/") ? "manifest" : "blob"
    }

    private func failure(
        kind: RishDemoPullFailure.Kind,
        code: String,
        message: String,
        retryable: Bool
    ) -> RishDemoPullFailure {
        RishDemoPullFailure(
            kind: kind,
            code: code,
            message: message,
            retryable: retryable
        )
    }
}
