import Foundation
import UIKit

enum RishPullPalette {
    static let background = UIColor(
        red: 7 / 255,
        green: 17 / 255,
        blue: 31 / 255,
        alpha: 1
    )
    static let card = UIColor(
        red: 16 / 255,
        green: 28 / 255,
        blue: 45 / 255,
        alpha: 1
    )
    static let field = UIColor(
        red: 10 / 255,
        green: 22 / 255,
        blue: 38 / 255,
        alpha: 1
    )
    static let accent = UIColor(
        red: 94 / 255,
        green: 234 / 255,
        blue: 212 / 255,
        alpha: 1
    )
    static let success = UIColor(
        red: 126 / 255,
        green: 231 / 255,
        blue: 135 / 255,
        alpha: 1
    )
    static let warning = UIColor(
        red: 251 / 255,
        green: 191 / 255,
        blue: 36 / 255,
        alpha: 1
    )
    static let danger = UIColor(
        red: 248 / 255,
        green: 113 / 255,
        blue: 113 / 255,
        alpha: 1
    )
    static let secondary = UIColor(
        red: 148 / 255,
        green: 163 / 255,
        blue: 184 / 255,
        alpha: 1
    )
    static let primary = UIColor(
        red: 241 / 255,
        green: 245 / 255,
        blue: 249 / 255,
        alpha: 1
    )
    static let border = UIColor(
        red: 38 / 255,
        green: 56 / 255,
        blue: 78 / 255,
        alpha: 1
    )
}

extension RishImagePullViewController {
    func makePlatformPickerCard() -> UIView {
        let card = makeCard(cornerRadius: 14)
        platformControl.selectedSegmentIndex = 0
        platformControl.selectedSegmentTintColor = RishPullPalette.accent
        platformControl.backgroundColor = RishPullPalette.field
        platformControl.setTitleTextAttributes(
            [.foregroundColor: RishPullPalette.background],
            for: .selected
        )
        platformControl.setTitleTextAttributes(
            [.foregroundColor: RishPullPalette.secondary],
            for: .normal
        )
        platformControl.accessibilityLabel = "Linux guest image platform"
        platformControl.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(platformControl)
        NSLayoutConstraint.activate([
            platformControl.topAnchor.constraint(equalTo: card.topAnchor, constant: 12),
            platformControl.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 12),
            platformControl.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -12),
            platformControl.bottomAnchor.constraint(equalTo: card.bottomAnchor, constant: -12),
            platformControl.heightAnchor.constraint(greaterThanOrEqualToConstant: 36),
        ])
        return card
    }

    func makeItemRow(_ item: RishDemoPullItem) -> UIView {
        let card = makeCard(cornerRadius: 14)
        let stack = UIStackView()
        stack.axis = .vertical
        stack.alignment = .fill
        stack.spacing = 7
        stack.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: card.topAnchor, constant: 13),
            stack.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 14),
            stack.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -14),
            stack.bottomAnchor.constraint(equalTo: card.bottomAnchor, constant: -13),
        ])

        let top = UIStackView()
        top.axis = .horizontal
        top.alignment = .firstBaseline
        top.spacing = 8
        top.addArrangedSubview(
            makeLabel(
                item.kind.uppercased(),
                style: .caption1,
                weight: .bold,
                color: RishPullPalette.secondary
            )
        )
        top.addArrangedSubview(UIView())
        top.addArrangedSubview(
            makeLabel(
                item.state.title,
                style: .caption2,
                weight: .bold,
                color: color(for: item.state)
            )
        )
        stack.addArrangedSubview(top)

        let digest = makeMonospacedLabel(
            style: .caption1,
            color: RishPullPalette.primary
        )
        digest.text = item.digest ?? "digest pending"
        digest.accessibilityLabel = "\(item.kind) digest"
        stack.addArrangedSubview(digest)

        let bytes = makeLabel(
            networkText(
                received: item.receivedBytes,
                total: item.totalBytes
            ),
            style: .caption2,
            weight: .semibold,
            color: RishPullPalette.secondary
        )
        stack.addArrangedSubview(bytes)

        if let total = item.totalBytes, total > 0,
           item.state == .downloading {
            let itemProgress = UIProgressView(progressViewStyle: .default)
            itemProgress.progressTintColor = RishPullPalette.accent
            itemProgress.trackTintColor = RishPullPalette.border
            itemProgress.progress = min(
                Float(item.receivedBytes) / Float(total),
                1
            )
            itemProgress.accessibilityLabel = "\(item.kind) download progress"
            itemProgress.accessibilityValue =
                "\(Int(itemProgress.progress * 100)) percent"
            stack.addArrangedSubview(itemProgress)
        }
        return card
    }

    func makeValueRow(label: String, value: String) -> UIView {
        let stack = UIStackView()
        stack.axis = .vertical
        stack.alignment = .fill
        stack.spacing = 3
        stack.addArrangedSubview(
            makeLabel(
                label,
                style: .caption2,
                weight: .bold,
                color: RishPullPalette.secondary
            )
        )
        stack.addArrangedSubview(
            makeLabel(
                value,
                style: .callout,
                weight: .semibold,
                color: RishPullPalette.primary
            )
        )
        return stack
    }

    func makeMonospacedValue(label: String, value: String) -> UIView {
        let stack = UIStackView()
        stack.axis = .vertical
        stack.alignment = .fill
        stack.spacing = 3
        stack.addArrangedSubview(
            makeLabel(
                label,
                style: .caption2,
                weight: .bold,
                color: RishPullPalette.secondary
            )
        )
        let valueLabel = makeMonospacedLabel(
            style: .caption1,
            color: RishPullPalette.primary
        )
        valueLabel.text = value
        stack.addArrangedSubview(valueLabel)
        return stack
    }

    func makeDivider() -> UIView {
        let divider = UIView()
        divider.backgroundColor = RishPullPalette.success
            .withAlphaComponent(0.22)
        divider.heightAnchor.constraint(equalToConstant: 1).isActive = true
        return divider
    }

    func makePill(_ text: String) -> UIView {
        let pill = UIView()
        pill.backgroundColor = RishPullPalette.accent.withAlphaComponent(0.1)
        pill.layer.borderColor =
            RishPullPalette.accent.withAlphaComponent(0.32).cgColor
        pill.layer.borderWidth = 1
        pill.layer.cornerRadius = 14
        let label = makeLabel(
            text,
            style: .caption2,
            weight: .bold,
            color: RishPullPalette.accent
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

    func makeSectionLabel(_ text: String) -> UILabel {
        let label = makeLabel(
            text,
            style: .caption1,
            weight: .bold,
            color: RishPullPalette.secondary
        )
        label.attributedText = trackedText(
            text,
            font: label.font,
            color: RishPullPalette.secondary,
            tracking: 1.4
        )
        return label
    }

    func makeCard(cornerRadius: CGFloat = 18) -> UIView {
        let card = UIView()
        card.backgroundColor = RishPullPalette.card
        card.layer.cornerRadius = cornerRadius
        card.layer.borderWidth = 1
        card.layer.borderColor = RishPullPalette.border.cgColor
        return card
    }

    func makeLabel(
        _ text: String,
        style: UIFont.TextStyle,
        weight: UIFont.Weight,
        color: UIColor,
        alignment: NSTextAlignment = .natural
    ) -> UILabel {
        let label = UILabel()
        configureLabel(
            label,
            text: text,
            style: style,
            weight: weight,
            color: color,
            alignment: alignment
        )
        return label
    }

    func configureLabel(
        _ label: UILabel,
        text: String,
        style: UIFont.TextStyle,
        weight: UIFont.Weight,
        color: UIColor,
        alignment: NSTextAlignment = .natural
    ) {
        label.font = scaledFont(style: style, weight: weight)
        label.adjustsFontForContentSizeCategory = true
        label.text = text
        label.textColor = color
        label.textAlignment = alignment
        label.numberOfLines = 0
        label.lineBreakMode = .byWordWrapping
    }

    func makeMonospacedLabel(
        style: UIFont.TextStyle,
        color: UIColor
    ) -> UILabel {
        let label = UILabel()
        let descriptor = UIFontDescriptor.preferredFontDescriptor(
            withTextStyle: style
        )
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

    func scaledFont(
        style: UIFont.TextStyle,
        weight: UIFont.Weight
    ) -> UIFont {
        let descriptor = UIFontDescriptor.preferredFontDescriptor(
            withTextStyle: style
        )
        return UIFont.systemFont(ofSize: descriptor.pointSize, weight: weight)
    }

    func trackedText(
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

    func networkText(received: UInt64, total: UInt64?) -> String {
        if let total {
            return "Network  \(formatBytes(received)) / \(formatBytes(total))"
        }
        return received == 0
            ? "Network  total pending"
            : "Network  \(formatBytes(received)) / total pending"
    }

    func formatBytes(_ bytes: UInt64) -> String {
        ByteCountFormatter.string(
            fromByteCount: Int64(clamping: bytes),
            countStyle: .file
        )
    }

    func color(for state: RishDemoPullItemState) -> UIColor {
        switch state {
        case .pending:
            return RishPullPalette.secondary
        case .downloading, .verifying:
            return RishPullPalette.accent
        case .cached, .stored:
            return RishPullPalette.success
        case .failed:
            return RishPullPalette.danger
        }
    }

    func removeAllArrangedSubviews(from stack: UIStackView) {
        for view in stack.arrangedSubviews {
            stack.removeArrangedSubview(view)
            view.removeFromSuperview()
        }
    }
}
