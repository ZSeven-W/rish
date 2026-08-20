import Foundation
import UIKit

@MainActor
final class RishImagePullViewController: UIViewController, UITextFieldDelegate {
    private let service: any RishDemoImagePulling
    private var generation = UUID()
    private var pulling = false
    private var cancelling = false
    private var attemptedAutoPull = false

    private let referenceField = UITextField()
    let platformControl = UISegmentedControl(items: ["ARM64 · v8", "AMD64"])
    private let actionButton = UIButton(type: .system)
    private let activityIndicator = UIActivityIndicatorView(style: .medium)
    private let progressView = UIProgressView(progressViewStyle: .default)
    private let phaseLabel = UILabel()
    private let phaseDetailLabel = UILabel()
    private let byteLabel = UILabel()
    private let verifiedLabel = UILabel()
    private let itemSection = UIStackView()
    private let itemStack = UIStackView()
    private let receiptCard = UIView()
    private let receiptStack = UIStackView()
    private let errorCard = UIView()
    private let errorTitleLabel = UILabel()
    private let errorMessageLabel = UILabel()
    private let errorCodeLabel = UILabel()

    init(service: any RishDemoImagePulling) {
        self.service = service
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
        view.backgroundColor = RishPullPalette.background
        configurePage()
        renderIdle()
    }

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        guard !attemptedAutoPull,
              let reference = RishDemoAutoPull.requestedReference(),
              let platform = RishDemoAutoPull.requestedPlatform() else {
            return
        }
        attemptedAutoPull = true
        referenceField.text = reference
        platformControl.selectedSegmentIndex =
            platform == .linuxAmd64 ? 1 : 0
        startPull()
    }

    func textFieldShouldReturn(_ textField: UITextField) -> Bool {
        textField.resignFirstResponder()
        if !pulling {
            startPull()
        }
        return true
    }

    private func configurePage() {
        let scrollView = UIScrollView()
        scrollView.alwaysBounceVertical = true
        scrollView.keyboardDismissMode = .interactive
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
                constant: -30
            ),
        ])

        content.addArrangedSubview(makeHeader())
        content.addArrangedSubview(
            makeLabel(
                "Pull an OCI image.\nVerify every byte.",
                style: .largeTitle,
                weight: .bold,
                color: RishPullPalette.primary
            )
        )
        content.addArrangedSubview(
            makeLabel(
                "Fetch a Linux image for a selected software-guest CPU into the app-owned content store.",
                style: .body,
                weight: .regular,
                color: RishPullPalette.secondary
            )
        )
        content.addArrangedSubview(makeBoundaryCard())
        content.setCustomSpacing(28, after: content.arrangedSubviews.last!)

        content.addArrangedSubview(makeSectionLabel("IMAGE REFERENCE"))
        content.setCustomSpacing(10, after: content.arrangedSubviews.last!)
        content.addArrangedSubview(makeInputCard())
        content.setCustomSpacing(20, after: content.arrangedSubviews.last!)
        content.addArrangedSubview(makeSectionLabel("GUEST PLATFORM"))
        content.setCustomSpacing(10, after: content.arrangedSubviews.last!)
        content.addArrangedSubview(makePlatformPickerCard())
        content.setCustomSpacing(12, after: content.arrangedSubviews.last!)
        configureActionButton()
        content.addArrangedSubview(actionButton)

        content.setCustomSpacing(28, after: actionButton)
        content.addArrangedSubview(makeSectionLabel("PULL SESSION"))
        content.setCustomSpacing(10, after: content.arrangedSubviews.last!)
        content.addArrangedSubview(makeProgressCard())

        configureItemSection()
        content.addArrangedSubview(itemSection)

        configureReceiptCard()
        content.addArrangedSubview(receiptCard)
        configureErrorCard()
        content.addArrangedSubview(errorCard)

        content.setCustomSpacing(26, after: errorCard)
        content.addArrangedSubview(
            makeLabel(
                "OCI Distribution  ·  SHA-256 verified  ·  App sandbox",
                style: .footnote,
                weight: .semibold,
                color: RishPullPalette.secondary,
                alignment: .center
            )
        )
    }

    private func makeHeader() -> UIView {
        let stack = UIStackView()
        stack.axis = .vertical
        stack.alignment = .leading
        stack.spacing = 8

        let top = UIStackView()
        top.axis = .horizontal
        top.alignment = .center
        top.spacing = 12
        top.addArrangedSubview(
            makeLabel(
                "rish",
                style: .title1,
                weight: .heavy,
                color: RishPullPalette.accent
            )
        )
        top.addArrangedSubview(UIView())
        top.addArrangedSubview(makePill("REAL REGISTRY I/O"))
        stack.addArrangedSubview(top)
        top.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true

        let tagline = makeLabel(
            "OCI CONTENT PIPELINE",
            style: .caption1,
            weight: .bold,
            color: RishPullPalette.secondary
        )
        tagline.attributedText = trackedText(
            "OCI CONTENT PIPELINE",
            font: tagline.font,
            color: RishPullPalette.secondary,
            tracking: 1.7
        )
        stack.addArrangedSubview(tagline)
        return stack
    }

    private func makeBoundaryCard() -> UIView {
        let card = makeCard()
        card.backgroundColor = RishPullPalette.warning.withAlphaComponent(0.07)
        card.layer.borderColor = RishPullPalette.warning
            .withAlphaComponent(0.28).cgColor

        let stack = UIStackView()
        stack.axis = .horizontal
        stack.alignment = .top
        stack.spacing = 12
        stack.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: card.topAnchor, constant: 15),
            stack.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 15),
            stack.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -15),
            stack.bottomAnchor.constraint(equalTo: card.bottomAnchor, constant: -15),
        ])

        let glyph = makeLabel(
            "↓",
            style: .headline,
            weight: .black,
            color: RishPullPalette.warning,
            alignment: .center
        )
        glyph.setContentHuggingPriority(.required, for: .horizontal)
        stack.addArrangedSubview(glyph)
        stack.addArrangedSubview(
            makeLabel(
                "Pulling downloads and verifies OCI content. It does not start a container or execute image binaries.",
                style: .footnote,
                weight: .medium,
                color: RishPullPalette.secondary
            )
        )
        return card
    }

    private func makeInputCard() -> UIView {
        let card = makeCard()
        card.backgroundColor = RishPullPalette.field

        referenceField.text = "alpine:latest"
        referenceField.placeholder = "registry/repository:tag"
        referenceField.textColor = RishPullPalette.primary
        referenceField.tintColor = RishPullPalette.accent
        referenceField.keyboardType = .URL
        referenceField.returnKeyType = .go
        referenceField.autocorrectionType = .no
        referenceField.autocapitalizationType = .none
        referenceField.clearButtonMode = .whileEditing
        referenceField.delegate = self
        referenceField.accessibilityLabel = "OCI image reference"
        let descriptor = UIFontDescriptor.preferredFontDescriptor(
            withTextStyle: .body
        )
        let base = UIFont.monospacedSystemFont(
            ofSize: descriptor.pointSize,
            weight: .medium
        )
        referenceField.font = UIFontMetrics(forTextStyle: .body)
            .scaledFont(for: base)
        referenceField.adjustsFontForContentSizeCategory = true
        referenceField.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(referenceField)
        NSLayoutConstraint.activate([
            referenceField.topAnchor.constraint(
                equalTo: card.topAnchor,
                constant: 16
            ),
            referenceField.leadingAnchor.constraint(
                equalTo: card.leadingAnchor,
                constant: 17
            ),
            referenceField.trailingAnchor.constraint(
                equalTo: card.trailingAnchor,
                constant: -17
            ),
            referenceField.bottomAnchor.constraint(
                equalTo: card.bottomAnchor,
                constant: -16
            ),
            referenceField.heightAnchor.constraint(greaterThanOrEqualToConstant: 28),
        ])
        return card
    }

    private func configureActionButton() {
        actionButton.setTitle("Pull & verify image", for: .normal)
        actionButton.setTitleColor(RishPullPalette.background, for: .normal)
        actionButton.backgroundColor = RishPullPalette.accent
        actionButton.layer.cornerRadius = 16
        actionButton.titleLabel?.font = scaledFont(
            style: .headline,
            weight: .bold
        )
        actionButton.titleLabel?.adjustsFontForContentSizeCategory = true
        actionButton.accessibilityHint =
            "Starts a real network pull from the image registry"
        actionButton.addTarget(
            self,
            action: #selector(actionTapped),
            for: .touchUpInside
        )
        actionButton.heightAnchor.constraint(
            greaterThanOrEqualToConstant: 54
        ).isActive = true
    }

    private func makeProgressCard() -> UIView {
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

        let phaseRow = UIStackView()
        phaseRow.axis = .horizontal
        phaseRow.alignment = .center
        phaseRow.spacing = 11
        activityIndicator.color = RishPullPalette.accent
        activityIndicator.hidesWhenStopped = true
        phaseRow.addArrangedSubview(activityIndicator)
        configureLabel(
            phaseLabel,
            text: "Ready",
            style: .headline,
            weight: .bold,
            color: RishPullPalette.primary
        )
        phaseRow.addArrangedSubview(phaseLabel)
        phaseRow.addArrangedSubview(UIView())
        stack.addArrangedSubview(phaseRow)

        configureLabel(
            phaseDetailLabel,
            text: "No network request has started.",
            style: .footnote,
            weight: .regular,
            color: RishPullPalette.secondary
        )
        stack.addArrangedSubview(phaseDetailLabel)

        progressView.progressTintColor = RishPullPalette.accent
        progressView.trackTintColor = RishPullPalette.border
        progressView.layer.cornerRadius = 2
        progressView.clipsToBounds = true
        progressView.accessibilityLabel = "Overall image pull progress"
        stack.addArrangedSubview(progressView)

        let stats = UIStackView()
        stats.axis = .vertical
        stats.alignment = .fill
        stats.spacing = 4
        configureLabel(
            byteLabel,
            text: "Network  —",
            style: .caption1,
            weight: .semibold,
            color: RishPullPalette.secondary
        )
        configureLabel(
            verifiedLabel,
            text: "Verified  —",
            style: .caption1,
            weight: .semibold,
            color: RishPullPalette.secondary
        )
        stats.addArrangedSubview(byteLabel)
        stats.addArrangedSubview(verifiedLabel)
        stack.addArrangedSubview(stats)
        return card
    }

    private func configureItemSection() {
        itemSection.axis = .vertical
        itemSection.alignment = .fill
        itemSection.spacing = 10
        itemSection.addArrangedSubview(makeSectionLabel("REQUESTS & BLOBS"))
        itemStack.axis = .vertical
        itemStack.alignment = .fill
        itemStack.spacing = 10
        itemSection.addArrangedSubview(itemStack)
        itemSection.isHidden = true
    }

    private func configureReceiptCard() {
        styleResultCard(
            receiptCard,
            color: RishPullPalette.success,
            stack: receiptStack
        )
        receiptCard.isHidden = true
    }

    private func configureErrorCard() {
        let stack = UIStackView()
        styleResultCard(errorCard, color: RishPullPalette.danger, stack: stack)
        configureLabel(
            errorTitleLabel,
            text: "Pull failed",
            style: .headline,
            weight: .bold,
            color: RishPullPalette.danger
        )
        configureLabel(
            errorMessageLabel,
            text: "",
            style: .body,
            weight: .regular,
            color: RishPullPalette.primary
        )
        errorMessageLabel.lineBreakMode = .byWordWrapping
        configureLabel(
            errorCodeLabel,
            text: "",
            style: .caption1,
            weight: .semibold,
            color: RishPullPalette.secondary
        )
        errorCodeLabel.lineBreakMode = .byCharWrapping
        stack.addArrangedSubview(errorTitleLabel)
        stack.addArrangedSubview(errorMessageLabel)
        stack.addArrangedSubview(errorCodeLabel)
        errorCard.isHidden = true
    }

    private func styleResultCard(
        _ card: UIView,
        color: UIColor,
        stack: UIStackView
    ) {
        card.backgroundColor = color.withAlphaComponent(0.08)
        card.layer.cornerRadius = 18
        card.layer.borderWidth = 1
        card.layer.borderColor = color.withAlphaComponent(0.35).cgColor
        stack.axis = .vertical
        stack.alignment = .fill
        stack.spacing = 10
        stack.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: card.topAnchor, constant: 17),
            stack.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 17),
            stack.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -17),
            stack.bottomAnchor.constraint(equalTo: card.bottomAnchor, constant: -17),
        ])
    }

    @objc
    private func actionTapped() {
        view.endEditing(true)
        if pulling {
            requestCancellation()
        } else {
            startPull()
        }
    }

    private func startPull() {
        let reference = referenceField.text?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        guard !reference.isEmpty else {
            renderFailure(
                RishDemoPullFailure(
                    kind: .invalidReference,
                    code: "empty_reference",
                    message: "Enter an OCI image reference before pulling.",
                    retryable: true
                )
            )
            return
        }

        let activeGeneration = UUID()
        let platform: RishImageGuestPlatform =
            platformControl.selectedSegmentIndex == 1
            ? .linuxAmd64 : .linuxArm64V8
        generation = activeGeneration
        pulling = true
        cancelling = false
        RishDemoPullResultStore.clear()
        referenceField.isEnabled = false
        platformControl.isEnabled = false
        errorCard.isHidden = true
        receiptCard.isHidden = true
        removeAllArrangedSubviews(from: itemStack)
        itemSection.isHidden = true
        renderRunning(
            title: "Starting pull",
            detail: "Opening a bounded registry session…",
            received: 0,
            total: nil,
            verified: 0
        )
        updateActionButton()

        service.pull(
            reference: reference,
            platform: platform,
            progress: { [weak self] progress in
                DispatchQueue.main.async {
                    guard let self, self.generation == activeGeneration else {
                        return
                    }
                    self.render(progress)
                }
            },
            completion: { [weak self] result in
                DispatchQueue.main.async {
                    guard let self, self.generation == activeGeneration else {
                        return
                    }
                    self.finish(result)
                }
            }
        )
    }

    private func requestCancellation() {
        guard pulling, !cancelling else {
            return
        }
        cancelling = true
        phaseLabel.text = "Cancelling safely"
        phaseDetailLabel.text =
            "Stopping network work and discarding uncommitted content…"
        service.cancel()
        updateActionButton()
    }

    private func finish(
        _ result: Result<RishDemoPullReceipt, RishDemoPullFailure>
    ) {
        pulling = false
        cancelling = false
        referenceField.isEnabled = true
        platformControl.isEnabled = true
        activityIndicator.stopAnimating()
        progressView.isHidden = false
        updateActionButton()

        switch result {
        case let .success(receipt):
            RishDemoPullResultStore.persistSuccess(receipt)
            renderReceipt(receipt)
        case let .failure(failure) where failure.kind == .cancelled:
            persist(failure)
            renderCancelled()
        case let .failure(failure):
            persist(failure)
            renderFailure(failure)
        }
    }

    private func persist(_ failure: RishDemoPullFailure) {
        RishDemoPullResultStore.persistFailure(
            reference: referenceField.text ?? "",
            failure: failure
        )
    }

    private func renderIdle() {
        pulling = false
        cancelling = false
        activityIndicator.stopAnimating()
        phaseLabel.text = "Ready"
        phaseDetailLabel.text =
            "No network request has started. The default resolves to Docker Hub."
        progressView.isHidden = true
        progressView.progress = 0
        byteLabel.text = "Network  —"
        verifiedLabel.text = "Verified  —"
        itemSection.isHidden = true
        receiptCard.isHidden = true
        errorCard.isHidden = true
        platformControl.isEnabled = true
        updateActionButton()
    }

    private func render(_ progress: RishDemoPullProgress) {
        guard pulling, !cancelling else {
            return
        }
        renderRunning(
            title: progress.phase.title,
            detail: progress.detail,
            received: progress.receivedBytes,
            total: progress.totalBytes,
            verified: progress.verifiedBytes
        )
        renderItems(progress.items)
    }

    private func renderRunning(
        title: String,
        detail: String,
        received: UInt64,
        total: UInt64?,
        verified: UInt64
    ) {
        phaseLabel.text = title
        phaseLabel.textColor = RishPullPalette.primary
        phaseDetailLabel.text = detail
        byteLabel.text = networkText(received: received, total: total)
        verifiedLabel.text = "Verified  \(formatBytes(verified))"

        if let total, total > 0 {
            activityIndicator.stopAnimating()
            progressView.isHidden = false
            progressView.progress = min(
                Float(received) / Float(total),
                1
            )
            progressView.accessibilityValue =
                "\(Int(progressView.progress * 100)) percent"
        } else {
            progressView.isHidden = true
            activityIndicator.startAnimating()
            progressView.accessibilityValue = "Total size not known yet"
        }
    }

    private func renderItems(_ items: [RishDemoPullItem]) {
        removeAllArrangedSubviews(from: itemStack)
        itemSection.isHidden = items.isEmpty
        for item in items {
            itemStack.addArrangedSubview(makeItemRow(item))
        }
    }

    private func renderReceipt(_ receipt: RishDemoPullReceipt) {
        phaseLabel.text = "Pulled & verified"
        phaseLabel.textColor = RishPullPalette.success
        phaseDetailLabel.text =
            "All descriptors passed size and SHA-256 verification."
        progressView.isHidden = false
        progressView.progress = 1
        byteLabel.text =
            "Network  \(formatBytes(receipt.downloadedBytes))"
        verifiedLabel.text =
            "Verified  \(formatBytes(receipt.verifiedBytes))"
        errorCard.isHidden = true
        receiptCard.isHidden = false
        removeAllArrangedSubviews(from: receiptStack)

        receiptStack.addArrangedSubview(
            makeLabel(
                "✓  CONTENT AVAILABLE",
                style: .headline,
                weight: .bold,
                color: RishPullPalette.success
            )
        )
        receiptStack.addArrangedSubview(
            makeMonospacedValue(
                label: "REFERENCE",
                value: receipt.canonicalReference
            )
        )
        receiptStack.addArrangedSubview(
            makeMonospacedValue(
                label: "RESOLVED DIGEST",
                value: receipt.resolvedDigest
            )
        )
        if let manifestDigest = receipt.manifestDigest {
            receiptStack.addArrangedSubview(
                makeMonospacedValue(
                    label: "MANIFEST",
                    value: manifestDigest
                )
            )
        }
        let platform = [
            receipt.operatingSystem,
            receipt.architecture,
            receipt.variant,
        ].compactMap { $0 }.joined(separator: "/")
        receiptStack.addArrangedSubview(
            makeValueRow(label: "PLATFORM", value: platform)
        )
        receiptStack.addArrangedSubview(
            makeValueRow(
                label: "LAYERS",
                value: String(receipt.layerCount)
            )
        )
        receiptStack.addArrangedSubview(
            makeValueRow(
                label: "VERIFIED",
                value: formatBytes(receipt.verifiedBytes)
            )
        )
        receiptStack.addArrangedSubview(
            makeMonospacedValue(
                label: "CAS PIN",
                value: receipt.casPin
            )
        )
        receiptStack.addArrangedSubview(makeDivider())
        receiptStack.addArrangedSubview(
            makeLabel(
                "The OCI content is stored in the app sandbox. No container was started and no image binary was executed.",
                style: .footnote,
                weight: .medium,
                color: RishPullPalette.secondary
            )
        )
    }

    private func renderCancelled() {
        phaseLabel.text = "Pull cancelled"
        phaseLabel.textColor = RishPullPalette.warning
        phaseDetailLabel.text =
            "Network work stopped. Uncommitted content was not published."
        progressView.isHidden = true
        errorCard.isHidden = true
        receiptCard.isHidden = true
    }

    private func renderFailure(_ failure: RishDemoPullFailure) {
        pulling = false
        cancelling = false
        referenceField.isEnabled = true
        platformControl.isEnabled = true
        activityIndicator.stopAnimating()
        phaseLabel.text = "Pull failed"
        phaseLabel.textColor = RishPullPalette.danger
        phaseDetailLabel.text = failure.message
        progressView.isHidden = true
        receiptCard.isHidden = true
        errorCard.isHidden = false
        errorTitleLabel.text = failure.retryable
            ? "Pull failed · retry available"
            : "Pull failed"
        errorMessageLabel.text = failure.message
        errorCodeLabel.text = "ERROR  \(failure.code)"
        updateActionButton()
    }

    private func updateActionButton() {
        if cancelling {
            actionButton.setTitle("Cancelling…", for: .normal)
            actionButton.backgroundColor =
                RishPullPalette.danger.withAlphaComponent(0.16)
            actionButton.setTitleColor(RishPullPalette.danger, for: .normal)
            actionButton.layer.borderColor =
                RishPullPalette.danger.withAlphaComponent(0.45).cgColor
            actionButton.layer.borderWidth = 1
            actionButton.isEnabled = false
        } else if pulling {
            actionButton.setTitle("Cancel pull", for: .normal)
            actionButton.backgroundColor =
                RishPullPalette.danger.withAlphaComponent(0.12)
            actionButton.setTitleColor(RishPullPalette.danger, for: .normal)
            actionButton.layer.borderColor =
                RishPullPalette.danger.withAlphaComponent(0.45).cgColor
            actionButton.layer.borderWidth = 1
            actionButton.isEnabled = true
        } else {
            actionButton.setTitle(
                errorCard.isHidden ? "Pull & verify image" : "Retry pull",
                for: .normal
            )
            actionButton.backgroundColor = RishPullPalette.accent
            actionButton.setTitleColor(RishPullPalette.background, for: .normal)
            actionButton.layer.borderWidth = 0
            actionButton.isEnabled = true
        }
    }

}
