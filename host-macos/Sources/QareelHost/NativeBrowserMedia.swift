@preconcurrency import AVFoundation
import AppKit
import WebKit

@MainActor
enum NativeBrowserMedia {
    enum Device: String {
        case camera
        case microphone

        var mediaType: AVMediaType { self == .camera ? .video : .audio }
        var title: String { rawValue.capitalized }
        var settingsURL: URL? {
            switch self {
            case .camera: URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Camera")
            case .microphone: URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
            }
        }
    }

    private static var granted: Set<String> = []

    static func isAllowed(site: String, devices: [Device]) -> Bool {
        devices.allSatisfy { granted.contains("\(site)|\($0.rawValue)") }
    }

    static func allow(site: String, devices: [Device]) {
        for device in devices { granted.insert("\(site)|\(device.rawValue)") }
    }

    static func names(_ devices: [Device]) -> String {
        devices.map(\.rawValue).joined(separator: " and ")
    }

    static func firstBlocked(_ devices: [Device]) async -> Device? {
        for device in devices where !(await hasAccess(device)) { return device }
        return nil
    }

    private static func hasAccess(_ device: Device) async -> Bool {
        switch AVCaptureDevice.authorizationStatus(for: device.mediaType) {
        case .authorized: true
        case .notDetermined: await AVCaptureDevice.requestAccess(for: device.mediaType)
        default: false
        }
    }
}

extension NativeBrowserPage {
    func webView(_ webView: WKWebView, requestMediaCapturePermissionFor origin: WKSecurityOrigin, initiatedByFrame frame: WKFrameInfo, type: WKMediaCaptureType, decisionHandler: @escaping @MainActor @Sendable (WKPermissionDecision) -> Void) {
        let devices: [NativeBrowserMedia.Device]
        switch type {
        case .camera: devices = [.camera]
        case .microphone: devices = [.microphone]
        case .cameraAndMicrophone: devices = [.camera, .microphone]
        @unknown default: devices = []
        }
        guard webView === self.webView, !devices.isEmpty else {
            decisionHandler(.deny)
            return
        }
        let site = "\(origin.protocol)://\(origin.host):\(origin.port)"
        let grant: @MainActor () -> Void = { [weak self] in
            Task { @MainActor in
                if let blocked = await NativeBrowserMedia.firstBlocked(devices) {
                    self?.showMediaBlocked(blocked)
                    decisionHandler(.deny)
                } else {
                    decisionHandler(.grant)
                }
            }
        }
        if NativeBrowserMedia.isAllowed(site: site, devices: devices) {
            grant()
            return
        }
        dialogs.enqueue(kind: .media, message: NativeBrowserMedia.names(devices), frame: frame) { answer in
            guard case .accepted = answer else {
                decisionHandler(.deny)
                return
            }
            NativeBrowserMedia.allow(site: site, devices: devices)
            grant()
        }
    }

    private func showMediaBlocked(_ device: NativeBrowserMedia.Device) {
        NSLog("browser media blocked by system privacy settings device=%@", device.rawValue)
        guard let window = webView.window, window !== renderWindow, window.isVisible else { return }
        let alert = NSAlert()
        alert.messageText = "\(device.title) access is off for CommissionAI"
        alert.informativeText = "Turn on CommissionAI in System Settings > Privacy & Security > \(device.title), then try again."
        alert.addButton(withTitle: "Open System Settings")
        alert.addButton(withTitle: "Cancel")
        alert.beginSheetModal(for: window) { response in
            if response == .alertFirstButtonReturn, let url = device.settingsURL { NSWorkspace.shared.open(url) }
        }
    }
}
