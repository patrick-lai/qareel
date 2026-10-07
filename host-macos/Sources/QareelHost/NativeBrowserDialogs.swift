import AppKit
import WebKit

@MainActor
final class NativeBrowserDialogs {
    enum Answer {
        case accepted(String)
        case files([URL])
        case cancelled
    }

    enum Kind {
        case alert
        case confirm
        case prompt(String)
        case files(multiple: Bool, directories: Bool)
        case media
    }

    private struct Request {
        let id: UUID
        let kind: Kind
        let origin: String
        let message: String
        let completion: @MainActor (Answer) -> Void
    }

    private weak var page: NativeBrowserPage?
    private var request: Request?
    private var presentationID: UUID?
    private var sheet: NSWindow?
    private var sheetObserver: NSObjectProtocol?
    private var active = true
    private var promptInput: NSTextField?
    private var promptDraft: String?
    private(set) var agentOwned = false

    init(page: NativeBrowserPage) { self.page = page }

    var pending: Bool { request != nil }
    var identifier: String? { request?.id.uuidString.lowercased() }

    func enqueue(kind: Kind, message: String, frame: WKFrameInfo, completion: @escaping @MainActor (Answer) -> Void) {
        guard active, request == nil, let page else { completion(.cancelled); return }
        let origin = frame.securityOrigin
        let source = origin.host.isEmpty ? "This page" : String(origin.host.prefix(512))
        request = Request(id: UUID(), kind: kind, origin: source, message: String(message.prefix(16384)), completion: completion)
        if case .prompt(let text) = kind { promptDraft = String(text.prefix(65536)) }
        agentOwned = !page.human
        page.interruptForUserAttention()
        page.queueState()
        presentIfVisible()
    }

    func presentIfVisible() {
        guard presentationID == nil, let request, let page,
              let window = page.webView.window, window !== page.renderWindow,
              window.isVisible, !page.webView.isHiddenOrHasHiddenAncestor else { return }
        if window.attachedSheet != nil {
            if sheetObserver == nil {
                sheetObserver = NotificationCenter.default.addObserver(forName: NSWindow.didEndSheetNotification, object: window, queue: .main) { [weak self] _ in
                    MainActor.assumeIsolated { self?.presentIfVisible() }
                }
            }
            return
        }
        stopObserving()
        let presentation = UUID()
        presentationID = presentation
        switch request.kind {
        case .files(let multiple, let directories):
            let panel = NSOpenPanel()
            panel.title = "Choose files for \(request.origin)"
            panel.allowsMultipleSelection = multiple
            panel.canChooseDirectories = directories
            panel.canChooseFiles = true
            sheet = panel
            panel.beginSheetModal(for: window) { [weak self] response in
                self?.finish(request.id, presentation: presentation, answer: response == .OK ? .files(panel.urls) : .cancelled)
            }
        default:
            let alert = NSAlert()
            if case .media = request.kind {
                alert.messageText = "Allow \(request.origin) to use your \(request.message)?"
                alert.informativeText = "It stays allowed for this site until you quit CommissionAI."
                alert.addButton(withTitle: "Allow")
                alert.addButton(withTitle: "Don't allow")
            } else {
                alert.messageText = "\(request.origin) says"
                alert.informativeText = request.message
                alert.addButton(withTitle: "OK")
            }
            var field: NSTextField?
            switch request.kind {
            case .prompt:
                let input = NSTextField(frame: NSRect(x: 0, y: 0, width: 320, height: 24))
                input.stringValue = promptDraft ?? ""
                alert.accessoryView = input
                promptInput = input
                field = input
                alert.addButton(withTitle: "Cancel")
            case .confirm: alert.addButton(withTitle: "Cancel")
            default: break
            }
            let input = field
            sheet = alert.window
            alert.beginSheetModal(for: window) { [weak self] response in
                self?.finish(request.id, presentation: presentation, answer: response == .alertFirstButtonReturn ? .accepted(input?.stringValue ?? "") : .cancelled)
            }
            if let input { alert.window.makeFirstResponder(input) }
        }
    }

    func perform(_ action: JSONValue) throws -> JSONValue {
        switch action["type"]?.stringValue {
        case "status": return describe()
        case "respond":
            guard let request else { throw NativeBrowserFailure(message: "browser.no_dialog: no dialog is open in this tab") }
            try requireAgentOwned()
            if case .files = request.kind { throw NativeBrowserFailure(message: "browser.dialog_kind: this is a file chooser; pass paths to choose files or dismiss it") }
            if case .media = request.kind { throw NativeBrowserFailure(message: "browser.dialog_user_only: only the user can allow a site to use the camera or microphone") }
            guard let accept = action["accept"]?.boolValue else { throw NativeBrowserFailure(message: "browser.protocol_invalid: missing accept flag") }
            let text = String((action["text"]?.stringValue ?? promptDraft ?? "").prefix(65536))
            let summary = describe()
            answer(accept ? .accepted(text) : .cancelled)
            return .object(["answered": .bool(true), "accepted": .bool(accept), "dialog": summary])
        case "files":
            guard let request else { throw NativeBrowserFailure(message: "browser.no_dialog: no dialog is open in this tab") }
            try requireAgentOwned()
            guard case .files(let multiple, _) = request.kind else { throw NativeBrowserFailure(message: "browser.dialog_kind: this dialog is not a file chooser") }
            guard case .array(let items)? = action["paths"], !items.isEmpty, items.count <= (multiple ? 20 : 1) else { throw NativeBrowserFailure(message: "browser.dialog_files: pass \(multiple ? "one to twenty file paths" : "exactly one file path")") }
            var urls: [URL] = []
            for item in items {
                guard let path = item.stringValue, path.hasPrefix("/"), FileManager.default.isReadableFile(atPath: path) else { throw NativeBrowserFailure(message: "browser.dialog_files: a chosen file is not readable") }
                urls.append(URL(fileURLWithPath: path))
            }
            let summary = describe()
            answer(.files(urls))
            return .object(["answered": .bool(true), "accepted": .bool(true), "dialog": summary])
        default: throw NativeBrowserFailure(message: "browser.protocol_invalid: unknown dialog action")
        }
    }

    private func requireAgentOwned() throws {
        guard agentOwned else { throw NativeBrowserFailure(message: "browser.dialog_user_owned: the user took over this tab before the dialog opened, so they answer it in the browser window") }
    }

    private func describe() -> JSONValue {
        guard let request else { return .object(["pending": .bool(false)]) }
        let kind: String
        var defaultText: JSONValue = .null
        switch request.kind {
        case .alert: kind = "alert"
        case .confirm: kind = "confirm"
        case .prompt(let text): kind = "prompt"; defaultText = .string(String(text.prefix(1024)))
        case .files(let multiple, _): kind = multiple ? "files_multiple" : "files"
        case .media: kind = "media"
        }
        return .object(["pending": .bool(true), "kind": .string(kind), "message": .string(String(request.message.prefix(2048))), "default_text": defaultText, "origin": .string(request.origin), "agent_owned": .bool(agentOwned)])
    }

    private func answer(_ answer: Answer) {
        guard let request else { return }
        suspend()
        self.request = nil
        presentationID = nil
        promptDraft = nil
        agentOwned = false
        request.completion(answer)
        page?.queueState()
    }

    func suspend() {
        stopObserving()
        if let promptInput { promptDraft = promptInput.stringValue }
        promptInput = nil
        presentationID = nil
        if let sheet {
            sheet.sheetParent?.endSheet(sheet, returnCode: .cancel)
            sheet.orderOut(nil)
        }
        sheet = nil
    }

    private func stopObserving() {
        if let sheetObserver { NotificationCenter.default.removeObserver(sheetObserver) }
        sheetObserver = nil
    }

    func cancel() {
        let previous = request
        request = nil
        suspend()
        promptDraft = nil
        previous?.completion(.cancelled)
        page?.queueState()
    }

    func invalidate() {
        active = false
        cancel()
    }

    private func finish(_ id: UUID, presentation: UUID, answer: Answer) {
        guard presentationID == presentation, let request, request.id == id else { return }
        self.request = nil
        presentationID = nil
        sheet = nil
        promptInput = nil
        promptDraft = nil
        request.completion(answer)
        page?.queueState()
    }
}

extension NativeBrowserPage {
    func webView(_ webView: WKWebView, runJavaScriptAlertPanelWithMessage message: String, initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping @MainActor @Sendable () -> Void) {
        dialogs.enqueue(kind: .alert, message: message, frame: frame) { _ in completionHandler() }
    }

    func webView(_ webView: WKWebView, runJavaScriptConfirmPanelWithMessage message: String, initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping @MainActor @Sendable (Bool) -> Void) {
        dialogs.enqueue(kind: .confirm, message: message, frame: frame) { answer in
            if case .accepted = answer { completionHandler(true) } else { completionHandler(false) }
        }
    }

    func webView(_ webView: WKWebView, runJavaScriptTextInputPanelWithPrompt prompt: String, defaultText: String?, initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping @MainActor @Sendable (String?) -> Void) {
        dialogs.enqueue(kind: .prompt(String((defaultText ?? "").prefix(65536))), message: prompt, frame: frame) { answer in
            if case .accepted(let text) = answer { completionHandler(text) } else { completionHandler(nil) }
        }
    }

    func webView(_ webView: WKWebView, runOpenPanelWith parameters: WKOpenPanelParameters, initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping @MainActor @Sendable ([URL]?) -> Void) {
        dialogs.enqueue(kind: .files(multiple: parameters.allowsMultipleSelection, directories: parameters.allowsDirectories), message: "", frame: frame) { answer in
            if case .files(let urls) = answer { completionHandler(urls) } else { completionHandler(nil) }
        }
    }
}
