import AppKit
import WebKit

public struct NativeBrowserFailure: LocalizedError {
    public let message: String
    public init(message: String) { self.message = message }
    public var errorDescription: String? { message }
}

@MainActor
final class NativeBrowserRenderWindow: NSWindow {
    var rendering = false {
        didSet {
            if oldValue != rendering {
                NotificationCenter.default.post(name: NSWindow.didChangeOcclusionStateNotification, object: self)
                NotificationCenter.default.post(name: rendering ? NSWindow.didBecomeKeyNotification : NSWindow.didResignKeyNotification, object: self)
            }
        }
    }

    override var isVisible: Bool { rendering }
    override var isKeyWindow: Bool { rendering }
    override var occlusionState: NSWindow.OcclusionState { rendering ? [.visible] : [] }
}

@MainActor
final class NativeBrowserWebView: WKWebView {
    var automationDepth = 0
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
}

@MainActor
final class NativeBrowserPage: NSObject, WKNavigationDelegate, WKUIDelegate {
    let id: String
    let workspaceID: String
    let profileID: UUID
    let webView: NativeBrowserWebView
    let renderWindow: NativeBrowserRenderWindow
    private(set) var controlEpoch: UInt64 = 0
    private(set) var human = false
    var hostGeneration: String?
    var onState: () -> Void = {}
    var onNavigation: () -> Void = {}
    var navigationAllowed: (URL?) -> Bool = { _ in true }
    var onTakeover: (UInt64) -> Void = { _ in }
    var onPopup: (WKWebViewConfiguration, URL?) -> WKWebView? = { _, _ in nil }
    var onClose: () -> Void = {}
    var onAutomationTarget: (CGRect, UInt64) -> Void = { _, _ in }
    var onRecordingPointer: (CGPoint, Bool) -> Void = { _, _ in }
    private var observations: [NSKeyValueObservation] = []
    private var activity = 0
    private var holding = false
    private var holdTask: Task<Void, Never>?
    private var holdDeadline = Date.distantPast
    private var loopNonce: String?
    private var inputSecond = 0
    private var inputCount = 0
    private var heldKeys: Set<String> = []
    private var modifierFlags: NSEvent.ModifierFlags = []
    private(set) var navigationEpoch: UInt64 = 0
    private(set) var viewportOverride: NSSize?
    lazy var automation = NativeBrowserAutomation(page: self)
    lazy var dialogs = NativeBrowserDialogs(page: self)
    lazy var console = NativeBrowserConsole(page: self)
    private var stateTask: Task<Void, Never>?
    private var pending: [UUID: CheckedContinuation<JSONValue, any Error>] = [:]
    private var timeouts: [UUID: Task<Void, Never>] = [:]
    private var popupRequestedURL: URL?
    static let frameWorld = WKContentWorld.world(name: "commission.frames")
    static let frameScript = "(()=>{if(window.top===window)return;const token=Math.random().toString(36).slice(2)+Date.now().toString(36);const post=()=>{try{window.webkit.messageHandlers.commissionFrame.postMessage({token,href:location.href,name:window.name||''});}catch(error){}};post();document.addEventListener('DOMContentLoaded',post,{once:true});})()"
    static let readableCanvas = "(()=>{const patch=(p)=>{if(!p||p.__commissionReadable)return;const get=p.getContext;Object.defineProperty(p,'__commissionReadable',{value:true});p.getContext=function(type,options){if(/^(webgl2?|experimental-webgl)$/.test(String(type))){options=Object.assign({},options,{preserveDrawingBuffer:true});}return get.call(this,type,options);};};patch(window.HTMLCanvasElement&&HTMLCanvasElement.prototype);patch(window.OffscreenCanvas&&OffscreenCanvas.prototype);})()"

    private static let safariApplicationName: String = {
        let version = ProcessInfo.processInfo.operatingSystemVersion
        return "Version/\(version.majorVersion).\(version.minorVersion) Safari/605.1.15"
    }()

    init(id: String, workspaceID: String, profileID: UUID, store: WKWebsiteDataStore, suppliedConfiguration: WKWebViewConfiguration? = nil, popupRequestedURL: URL? = nil) {
        self.id = id
        self.workspaceID = workspaceID
        self.profileID = profileID
        self.popupRequestedURL = popupRequestedURL
        let configuration = suppliedConfiguration ?? WKWebViewConfiguration()
        if suppliedConfiguration == nil {
            configuration.websiteDataStore = store
            configuration.applicationNameForUserAgent = Self.safariApplicationName
        }
        let contentController = WKUserContentController()
        if !contentController.userScripts.contains(where: { $0.source == Self.readableCanvas }) {
            contentController.addUserScript(WKUserScript(source: Self.readableCanvas, injectionTime: .atDocumentStart, forMainFrameOnly: false, in: .page))
        }
        let inputBridge = NativeBrowserInputBridge()
        contentController.add(inputBridge, contentWorld: .page, name: "commissionInput")
        contentController.add(inputBridge, contentWorld: Self.frameWorld, name: "commissionFrame")
        if !contentController.userScripts.contains(where: { $0.source == Self.frameScript }) {
            contentController.addUserScript(WKUserScript(source: Self.frameScript, injectionTime: .atDocumentStart, forMainFrameOnly: false, in: Self.frameWorld))
        }
        for script in configuration.userContentController.userScripts where !NativeBrowserConsole.owns(script) { contentController.addUserScript(script) }
        configuration.userContentController = contentController
        configuration.preferences.isElementFullscreenEnabled = true
        configuration.preferences.inactiveSchedulingPolicy = .throttle
        configuration.preferences.javaScriptCanOpenWindowsAutomatically = false
        webView = NativeBrowserWebView(frame: NSRect(x: 0, y: 0, width: 1280, height: 800), configuration: configuration)
        renderWindow = NativeBrowserRenderWindow(contentRect: webView.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        renderWindow.isReleasedWhenClosed = false
        renderWindow.orderOut(nil)
        super.init()
        inputBridge.page = self
        webView.navigationDelegate = self
        webView.uiDelegate = self
        webView.allowsBackForwardNavigationGestures = true
        webView.allowsMagnification = true
        renderWindow.contentView?.addSubview(webView)
        observations = [
            webView.observe(\.url) { [weak self] _, _ in Task { @MainActor in self?.queueState() } },
            webView.observe(\.title) { [weak self] _, _ in Task { @MainActor in self?.queueState() } },
            webView.observe(\.isLoading) { [weak self] _, _ in Task { @MainActor in self?.queueState() } }
        ]
    }

    var state: JSONValue {
        let actualURL = webView.url?.absoluteString ?? "about:blank"
        let url = actualURL == "about:blank" ? popupRequestedURL?.absoluteString ?? actualURL : actualURL
        return .object([
            "url": .string(url),
            "title": .string(webView.title ?? ""),
            "loading": .bool(webView.isLoading),
            "can_go_back": .bool(webView.canGoBack),
            "can_go_forward": .bool(webView.canGoForward),
            "control_epoch": .number(Double(controlEpoch)),
            "human": .bool(human),
            "attention": dialogs.pending ? .string("This tab is waiting for a dialog. Open it to continue.") : .null,
            "attention_id": dialogs.identifier.map(JSONValue.string) ?? .null
        ])
    }

    func queueState() {
        guard stateTask == nil else { return }
        stateTask = Task { [weak self] in
            await Task.yield()
            guard let self else { return }
            stateTask = nil
            onState()
        }
    }

    func takeover() {
        guard !human else { return }
        loopNonce = nil
        releaseInputs()
        human = true
        controlEpoch += 1
        onTakeover(controlEpoch)
        queueState()
    }

    func control(epoch: UInt64, human requested: Bool) throws {
        if !requested && dialogs.pending { throw NativeBrowserFailure(message: "browser.user_attention_required: answer the pending dialog before resuming automation") }
        guard epoch > controlEpoch else { throw NativeBrowserFailure(message: "browser.stale_control: refresh the tab before changing control") }
        controlEpoch = epoch
        human = requested
        queueState()
    }

    func validate(agent: Bool, epoch: UInt64, deadline: Double, generation: String, allowingDialog: Bool = false) throws {
        guard hostGeneration == generation else { throw NativeBrowserFailure(message: "browser.host_disconnected: command belongs to an expired connection") }
        guard Date().timeIntervalSince1970 * 1000 <= deadline else { throw NativeBrowserFailure(message: "browser.command_expired: command did not execute before its deadline") }
        if agent && dialogs.pending && !allowingDialog { throw NativeBrowserFailure(message: "browser.dialog_pending: a dialog is open in this tab; answer it with handle_dialog") }
        if agent && (human || controlEpoch != epoch) {
            throw NativeBrowserFailure(message: "browser.human_driving: the user has taken control of this tab")
        }
    }

    func beginActivity() {
        activity += 1
        webView.configuration.preferences.inactiveSchedulingPolicy = .none
        renderWindow.rendering = true
    }

    func hold(milliseconds: Int, input: String? = nil) {
        holdTask?.cancel()
        holdTask = nil
        guard milliseconds > 0 else { releaseHold(); return }
        let deadline = max(holdDeadline, Date().addingTimeInterval(Double(milliseconds) / 1000))
        holdDeadline = deadline
        if let input, input.count >= 16, input.count <= 64 { loopNonce = input }
        if !holding { holding = true; beginActivity() }
        holdTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(max(0, deadline.timeIntervalSinceNow)))
            guard !Task.isCancelled, let self, self.holding else { return }
            self.holdTask = nil
            self.releaseHold()
        }
    }

    private func releaseHold() {
        holdDeadline = .distantPast
        loopNonce = nil
        releaseInputs()
        if holding { holding = false; endActivity() }
    }

    func releaseInputs() {
        for name in Array(heldKeys) { try? trustedKey(phase: "up", name: name) }
        heldKeys.removeAll()
        modifierFlags = []
        if heldTarget != nil, let window = webView.window, let event = NSEvent.mouseEvent(with: .leftMouseUp, location: .zero, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber, context: nil, eventNumber: heldEventNumber, clickCount: 1, pressure: 0) {
            webView.automationDepth += 1
            heldTarget?.mouseUp(with: event)
            webView.automationDepth -= 1
        }
        heldTarget = nil
    }

    func wheel(x: Double, y: Double, dx: Double, dy: Double, zoom: Bool) throws {
        try validatePointerScale()
        guard let window = webView.window, let screen = window.screen ?? NSScreen.screens.first else { throw NativeBrowserFailure(message: "browser.target_unavailable: tab has no native view") }
        let scale = webView.pageZoom
        let local = NSPoint(x: x * scale, y: webView.isFlipped ? y * scale : webView.bounds.height - y * scale)
        guard webView.bounds.contains(local) else { throw NativeBrowserFailure(message: "browser.target_unavailable: the point is outside the page") }
        let onScreen = window.convertPoint(toScreen: webView.convert(local, to: nil))
        guard let event = CGEvent(scrollWheelEvent2Source: nil, units: .pixel, wheelCount: 2, wheel1: Int32(max(-20000, min(20000, -dy)).rounded()), wheel2: Int32(max(-20000, min(20000, -dx)).rounded()), wheel3: 0) else {
            throw NativeBrowserFailure(message: "browser.input_failed: could not create a wheel event")
        }
        event.location = CGPoint(x: onScreen.x, y: screen.frame.maxY - onScreen.y)
        event.flags = zoom ? .maskControl : []
        event.setIntegerValueField(.mouseEventWindowUnderMousePointer, value: Int64(window.windowNumber))
        event.setIntegerValueField(.mouseEventWindowUnderMousePointerThatCanHandleThisEvent, value: Int64(window.windowNumber))
        guard let wheel = NSEvent(cgEvent: event) else { throw NativeBrowserFailure(message: "browser.input_failed: could not create a wheel event") }
        webView.automationDepth += 1
        defer { webView.automationDepth -= 1 }
        webView.scrollWheel(with: wheel)
    }

    func receiveLoopInput(_ body: Any) {
        guard let message = body as? [String: Any], let nonce = message["nonce"] as? String, let expected = loopNonce, nonce == expected, holding, !human, !dialogs.pending else { return }
        let second = Int(Date().timeIntervalSince1970)
        if second != inputSecond { inputSecond = second; inputCount = 0 }
        inputCount += 1
        guard inputCount <= 600 else { return }
        let phase = message["phase"] as? String ?? ""
        switch message["kind"] as? String {
        case "key":
            guard let name = message["key"] as? String, name.count <= 32 else { return }
            if phase == "tap" { try? trustedKey(phase: "down", name: name); try? trustedKey(phase: "up", name: name) }
            else { try? trustedKey(phase: phase, name: name) }
        case "wheel":
            guard let x = message["x"] as? Double, let y = message["y"] as? Double else { return }
            try? wheel(x: x, y: y, dx: message["dx"] as? Double ?? 0, dy: message["dy"] as? Double ?? 0, zoom: message["zoom"] as? Bool ?? false)
        case "pointer":
            guard let x = message["x"] as? Double, let y = message["y"] as? Double else { return }
            switch phase {
            case "tap": try? pointer(.object(["x": .number(x), "y": .number(y)]), click: true)
            case "move" where heldTarget == nil: try? pointer(.object(["x": .number(x), "y": .number(y)]), click: false)
            case "down", "move", "up": try? heldPointer(phase: phase, x: x, y: y)
            default: return
            }
        default: return
        }
    }

    func endActivity() {
        activity = max(0, activity - 1)
        if activity == 0 {
            renderWindow.rendering = false
            webView.configuration.preferences.inactiveSchedulingPolicy = .throttle
        }
    }

    func hide() {
        dialogs.suspend()
        guard webView.window !== renderWindow else { return }
        webView.removeFromSuperview()
        webView.frame = NSRect(origin: .zero, size: webView.frame.size)
        webView.bounds = NSRect(origin: .zero, size: webView.frame.size)
        renderWindow.setContentSize(webView.frame.size)
        renderWindow.contentView?.addSubview(webView)
    }

    func navigate(_ text: String) throws {
        guard let url = URL(string: text), ["http", "https", "about"].contains(url.scheme?.lowercased() ?? ""), url.scheme != "about" || text == "about:blank" else {
            throw NativeBrowserFailure(message: "browser.url_invalid: expected an HTTP or HTTPS address")
        }
        guard navigationAllowed(url) else { throw NativeBrowserFailure(message: "browser.remote_localhost_unavailable: devbox loopback URLs require explicit port forwarding") }
        dialogs.cancel()
        popupRequestedURL = nil
        navigationEpoch += 1
        webView.load(URLRequest(url: url))
    }

    private func resolve(_ id: UUID, _ result: Result<JSONValue, any Error>) {
        timeouts.removeValue(forKey: id)?.cancel()
        pending.removeValue(forKey: id)?.resume(with: result)
    }

    func interruptForUserAttention() {
        for id in Array(pending.keys) {
            resolve(id, .failure(NativeBrowserFailure(message: "browser.dialog_pending: a dialog opened; answer it with handle_dialog (the action that opened it already ran)")))
        }
    }

    func disconnect() {
        console.invalidate()
        hostGeneration = nil
        for id in Array(pending.keys) {
            resolve(id, .failure(NativeBrowserFailure(message: "browser.host_disconnected: operation outcome is unknown")))
        }
        holdTask?.cancel()
        holdTask = nil
        releaseInputs()
        loopNonce = nil
        holdDeadline = .distantPast
        holding = false
        activity = 0
        endActivity()
    }

    private func awaitResult(_ start: (@escaping @MainActor (Result<JSONValue, any Error>) -> Void) -> Void) async throws -> JSONValue {
        try await withCheckedThrowingContinuation { continuation in
            let id = UUID()
            pending[id] = continuation
            timeouts[id] = Task { [weak self] in
                do { try await Task.sleep(for: .seconds(12)) } catch { return }
                self?.resolve(id, .failure(NativeBrowserFailure(message: "browser.process_unresponsive: native operation timed out; its outcome is unknown")))
            }
            start { [weak self] result in self?.resolve(id, result) }
        }
    }

    private var frames: [(token: String, url: String, name: String, info: WKFrameInfo)] = []

    func registerFrame(_ body: Any, info: WKFrameInfo) {
        guard !info.isMainFrame, let message = body as? [String: Any], let token = message["token"] as? String, token.count <= 64, let url = message["href"] as? String, url.count <= 4096 else { return }
        let name = (message["name"] as? String).map { String($0.prefix(200)) } ?? ""
        frames.removeAll { $0.token == token }
        frames.append((token, url, name, info))
        if frames.count > 64 { frames.removeFirst(frames.count - 64) }
    }

    func frameList() -> JSONValue {
        .array(frames.map { .object(["id": .string($0.token), "url": .string($0.url), "name": .string($0.name)]) })
    }

    func evaluate(_ script: String, frame: String) async throws -> JSONValue {
        guard let match = frames.last(where: { $0.token == frame }) ?? frames.last(where: { $0.name == frame }) ?? frames.last(where: { $0.url.contains(frame) }) else {
            throw NativeBrowserFailure(message: "browser.frame_unavailable: no frame matches \(frame); list frames with browser_evaluate frame=list")
        }
        return try await evaluate(script, frameInfo: match.info)
    }

    func evaluate(_ script: String, isolated: Bool = false, world: WKContentWorld? = nil, frameInfo: WKFrameInfo? = nil) async throws -> JSONValue {
        guard !dialogs.pending else { throw NativeBrowserFailure(message: "browser.dialog_pending: a dialog is open in this tab; answer it with handle_dialog, or the user can answer it in the window") }
        return try await awaitResult { completion in
            webView.callAsyncJavaScript("return await (\(script));", arguments: [:], in: frameInfo, in: world ?? (isolated ? NativeBrowserScript.world : .page)) { result in
                do {
                    let value = try result.get()
                    let data = try JSONSerialization.data(withJSONObject: value, options: [.fragmentsAllowed])
                    guard data.count <= 4 * 1024 * 1024, let decoded = JSONValue.parse(data) else {
                        throw NativeBrowserFailure(message: "browser.output_limit: browser result exceeds four MiB")
                    }
                    completion(.success(decoded))
                } catch {
                    let details = (error as NSError).userInfo
                    if let message = details["WKJavaScriptExceptionMessage"] as? String, !message.isEmpty {
                        let line = (details["WKJavaScriptExceptionLineNumber"] as? NSNumber).map { " (line \($0))" } ?? ""
                        completion(.failure(NativeBrowserFailure(message: String("\(message)\(line)".prefix(2000)))))
                    } else {
                        completion(.failure(error))
                    }
                }
            }
        }
    }

    func click(reference: String, agent: Bool, epoch: UInt64, deadline: Double, generation: String) async throws {
        let navigation = navigationEpoch
        let point = try await evaluate(NativeBrowserScript.target(reference: reference, editing: false), isolated: true)
        try validate(agent: agent, epoch: epoch, deadline: deadline, generation: generation)
        guard navigation == navigationEpoch else { throw NativeBrowserFailure(message: "browser.stale_target: page navigated during target lookup") }
        try pointer(point, click: true)
    }

    func validatePointerScale() throws {
        guard abs(webView.magnification - 1) < 0.0001 else {
            throw NativeBrowserFailure(message: "browser.stale: trusted pointer automation is unavailable while pinch zoom is active; reset pinch zoom manually before resuming")
        }
    }

    func recordAutomationTarget(_ value: JSONValue) {
        guard let bounds = value["bounds"], let left = bounds["left"]?.doubleValue, let top = bounds["top"]?.doubleValue, let width = bounds["width"]?.doubleValue, let height = bounds["height"]?.doubleValue, [left, top, width, height].allSatisfy({ $0.isFinite && $0 >= 0 }), width > 0, height > 0 else { return }
        let scale = webView.pageZoom
        let rect = CGRect(x: left * scale, y: top * scale, width: width * scale, height: height * scale).intersection(CGRect(origin: .zero, size: webView.bounds.size))
        guard !rect.isEmpty, !rect.isNull else { return }
        onAutomationTarget(rect, navigationEpoch)
    }

    func pointer(_ point: JSONValue, click: Bool) throws {
        try validatePointerScale()
        guard let x = point["x"]?.doubleValue, let y = point["y"]?.doubleValue, x.isFinite, y.isFinite, let window = webView.window else {
            throw NativeBrowserFailure(message: "browser.target_unavailable: target has no native view")
        }
        let scale = webView.pageZoom
        let local = NSPoint(x: x * scale, y: webView.isFlipped ? y * scale : webView.bounds.height - y * scale)
        guard webView.bounds.contains(local), let parent = webView.superview, let target = webView.hitTest(webView.convert(local, to: parent)) else {
            throw NativeBrowserFailure(message: "browser.target_unavailable: native hit test failed")
        }
        let location = webView.convert(local, to: nil)
        let types: [NSEvent.EventType] = click ? [.leftMouseDown, .leftMouseUp] : [.mouseMoved]
        let events = try types.map { type in
            guard let event = NSEvent.mouseEvent(with: type, location: location, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: click ? 1 : 0, pressure: type == .leftMouseDown ? 1 : 0) else {
                throw NativeBrowserFailure(message: "browser.input_failed: could not create a pointer event")
            }
            return event
        }
        var hoverOwner: NSObject?
        if !click {
            let owners = webView.trackingAreas.filter { $0.options.contains(.mouseMoved) }.compactMap { $0.owner as? NSObject }.filter { $0.responds(to: #selector(NSResponder.mouseMoved(with:))) }
            guard let owner = owners.first, owners.allSatisfy({ $0 === owner }) else {
                throw NativeBrowserFailure(message: "browser.input_failed: native hover tracking is unavailable")
            }
            hoverOwner = owner
        }
        webView.automationDepth += 1
        defer { webView.automationDepth -= 1 }
        if click { recordAutomationTarget(point) }
        onRecordingPointer(CGPoint(x: x * scale, y: y * scale), click)
        for event in events {
            switch event.type {
            case .leftMouseDown: target.mouseDown(with: event)
            case .leftMouseUp: target.mouseUp(with: event)
            default: hoverOwner?.perform(#selector(NSResponder.mouseMoved(with:)), with: event)
            }
        }
    }

    private var heldTarget: NSView?
    var pointerHeld: Bool { heldTarget != nil }
    private var heldEventNumber = 0

    func heldPointer(phase: String, x: Double, y: Double) throws {
        try validatePointerScale()
        guard x.isFinite, y.isFinite, let window = webView.window else { throw NativeBrowserFailure(message: "browser.target_unavailable: target has no native view") }
        let scale = webView.pageZoom
        let local = NSPoint(x: x * scale, y: webView.isFlipped ? y * scale : webView.bounds.height - y * scale)
        guard webView.bounds.contains(local), let parent = webView.superview else { throw NativeBrowserFailure(message: "browser.target_unavailable: the point is outside the page") }
        let location = webView.convert(local, to: nil)
        let type: NSEvent.EventType
        switch phase {
        case "down":
            guard heldTarget == nil else { throw NativeBrowserFailure(message: "browser.input_failed: the pointer is already down; release it first") }
            type = .leftMouseDown
            guard let target = webView.hitTest(webView.convert(local, to: parent)) else { throw NativeBrowserFailure(message: "browser.target_unavailable: native hit test failed") }
            heldTarget = target
            heldEventNumber += 1
        case "move":
            type = .leftMouseDragged
        case "up":
            type = .leftMouseUp
        default: throw NativeBrowserFailure(message: "browser.protocol_invalid: unknown pointer phase")
        }
        guard let target = heldTarget else { throw NativeBrowserFailure(message: "browser.input_failed: the pointer is not down") }
        guard let event = NSEvent.mouseEvent(with: type, location: location, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber, context: nil, eventNumber: heldEventNumber, clickCount: 1, pressure: type == .leftMouseUp ? 0 : 1) else {
            heldTarget = nil
            throw NativeBrowserFailure(message: "browser.input_failed: could not create a pointer event")
        }
        webView.automationDepth += 1
        defer { webView.automationDepth -= 1 }
        onRecordingPointer(CGPoint(x: x * scale, y: y * scale), phase == "down")
        switch type {
        case .leftMouseDown: target.mouseDown(with: event)
        case .leftMouseDragged: target.mouseDragged(with: event)
        default:
            target.mouseUp(with: event)
            heldTarget = nil
        }
    }

    func releaseHeldPointer() { heldTarget = nil }

    func resize(width: Int?, height: Int?) throws {
        if let width, let height, (240...3840).contains(width), (240...2160).contains(height) {
            viewportOverride = NSSize(width: width, height: height)
        } else if width == nil && height == nil {
            viewportOverride = nil
        } else {
            throw NativeBrowserFailure(message: "browser.viewport_invalid: provide both bounded dimensions or reset both")
        }
        automation.invalidate()
        let size = viewportOverride ?? NSSize(width: 1280, height: 800)
        webView.frame.size = NSSize(width: size.width * webView.pageZoom, height: size.height * webView.pageZoom)
        if webView.window === renderWindow { renderWindow.setContentSize(webView.frame.size) }
    }

    func insertPreparedText(_ text: String) throws {
        guard text.utf8.count <= 65536 else { throw NativeBrowserFailure(message: "browser.input_limit: text exceeds 64 KiB") }
        if text.isEmpty { try press("Backspace") }
        else { try key(text, code: 0) }
    }

    func type(text: String, reference: String?, agent: Bool, epoch: UInt64, deadline: Double, generation: String) async throws {
        guard text.utf8.count <= 65536 else { throw NativeBrowserFailure(message: "browser.input_limit: text exceeds 64 KiB") }
        let navigation = navigationEpoch
        let target: JSONValue
        if let reference {
            target = try await evaluate(NativeBrowserScript.target(reference: reference, editing: true), isolated: true)
        } else {
            target = try await evaluate(NativeBrowserScript.focused, isolated: true)
        }
        try validate(agent: agent, epoch: epoch, deadline: deadline, generation: generation)
        guard navigation == navigationEpoch else { throw NativeBrowserFailure(message: "browser.stale_target: page navigated during focus lookup") }
        recordAutomationTarget(target)
        if text.isEmpty {
            if reference != nil { try press("Backspace") }
        } else {
            try key(text, code: 0)
        }
    }

    private static let keyCodes: [String: UInt16] = ["a": 0, "s": 1, "d": 2, "f": 3, "h": 4, "g": 5, "z": 6, "x": 7, "c": 8, "v": 9, "b": 11, "q": 12, "w": 13, "e": 14, "r": 15, "y": 16, "t": 17, "1": 18, "2": 19, "3": 20, "4": 21, "6": 22, "5": 23, "=": 24, "9": 25, "7": 26, "-": 27, "8": 28, "0": 29, "]": 30, "o": 31, "u": 32, "[": 33, "i": 34, "p": 35, "l": 37, "j": 38, "'": 39, "k": 40, ";": 41, "\\": 42, ",": 43, "/": 44, "n": 45, "m": 46, ".": 47, "`": 50]
    private static let namedKeys: [String: (String, UInt16)] = ["Enter": ("\r", 36), "Tab": ("\t", 48), "Escape": ("\u{1b}", 53), "Backspace": ("\u{8}", 51), "Delete": ("\u{f728}", 117), "ArrowLeft": ("\u{f702}", 123), "ArrowRight": ("\u{f703}", 124), "ArrowDown": ("\u{f701}", 125), "ArrowUp": ("\u{f700}", 126), "Space": (" ", 49), "Home": ("\u{f729}", 115), "End": ("\u{f72b}", 119), "PageUp": ("\u{f72c}", 116), "PageDown": ("\u{f72d}", 121), "F1": ("\u{f704}", 122), "F2": ("\u{f705}", 120), "F3": ("\u{f706}", 99), "F4": ("\u{f707}", 118), "F5": ("\u{f708}", 96), "F6": ("\u{f709}", 97), "F7": ("\u{f70a}", 98), "F8": ("\u{f70b}", 100), "F9": ("\u{f70c}", 101), "F10": ("\u{f70d}", 109), "F11": ("\u{f70e}", 103), "F12": ("\u{f70f}", 111)]
    private static let modifierKeys: [String: (NSEvent.ModifierFlags, UInt16)] = ["Shift": (.shift, 56), "Control": (.control, 59), "Alt": (.option, 58), "Meta": (.command, 55)]

    func trustedKey(phase: String, name: String) throws {
        guard phase == "down" || phase == "up" else { throw NativeBrowserFailure(message: "browser.protocol_invalid: key phase must be down or up") }
        guard let window = webView.window else { throw NativeBrowserFailure(message: "browser.input_failed: tab has no native host") }
        let down = phase == "down"
        let event: NSEvent?
        if let (flag, code) = Self.modifierKeys[name] {
            if down { modifierFlags.insert(flag) } else { modifierFlags.remove(flag) }
            event = NSEvent.keyEvent(with: .flagsChanged, location: .zero, modifierFlags: modifierFlags, timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber, context: nil, characters: "", charactersIgnoringModifiers: "", isARepeat: false, keyCode: code)
        } else {
            let lower = name.count == 1 ? name.lowercased() : name
            guard let (characters, code) = Self.namedKeys[name] ?? Self.keyCodes[lower].map({ (modifierFlags.contains(.shift) ? name.uppercased() : lower, $0) }) else {
                throw NativeBrowserFailure(message: "browser.key_unavailable: \(name) is not a key this browser can hold")
            }
            let repeating = down && heldKeys.contains(name)
            event = NSEvent.keyEvent(with: down ? .keyDown : .keyUp, location: .zero, modifierFlags: modifierFlags, timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber, context: nil, characters: characters, charactersIgnoringModifiers: lower == name ? characters : lower, isARepeat: repeating, keyCode: code)
        }
        guard let event else { throw NativeBrowserFailure(message: "browser.input_failed: could not create a keyboard event") }
        if down { heldKeys.insert(name) } else { heldKeys.remove(name) }
        webView.automationDepth += 1
        defer { webView.automationDepth -= 1 }
        if window.firstResponder !== webView { window.makeFirstResponder(webView) }
        window.sendEvent(event)
    }

    func press(_ name: String) throws {
        let keys: [String: (String, UInt16)] = ["Enter": ("\r", 36), "Tab": ("\t", 48), "Escape": ("\u{1b}", 53), "Backspace": ("\u{8}", 51), "Delete": ("\u{f728}", 117), "ArrowLeft": ("\u{f702}", 123), "ArrowRight": ("\u{f703}", 124), "ArrowDown": ("\u{f701}", 125), "ArrowUp": ("\u{f700}", 126), "Space": (" ", 49)]
        guard let (characters, code) = keys[name] else { throw NativeBrowserFailure(message: "browser.key_unavailable: this native key is not implemented") }
        try key(characters, code: code)
    }

    private func key(_ characters: String, code: UInt16) throws {
        guard let window = webView.window else { throw NativeBrowserFailure(message: "browser.input_failed: tab has no native host") }
        webView.automationDepth += 1
        defer { webView.automationDepth -= 1 }
        window.makeFirstResponder(webView)
        for type in [NSEvent.EventType.keyDown, .keyUp] {
            guard let event = NSEvent.keyEvent(with: type, location: .zero, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber, context: nil, characters: characters, charactersIgnoringModifiers: characters, isARepeat: false, keyCode: code) else {
                throw NativeBrowserFailure(message: "browser.input_failed: could not create a keyboard event")
            }
            window.sendEvent(event)
        }
    }

    func screenshot() async throws -> JSONValue {
        let configuration = WKSnapshotConfiguration()
        let viewport = webView.bounds.size
        let backing = max(1, webView.window?.backingScaleFactor ?? renderWindow.backingScaleFactor)
        configuration.snapshotWidth = NSNumber(value: Double(min(max(viewport.width, 320), 1280) / backing))
        return try await awaitResult { completion in
            webView.takeSnapshot(with: configuration) { image, error in
                if let error { completion(.failure(error)); return }
                guard let image, let cgImage = image.cgImage(forProposedRect: nil, context: nil, hints: nil),
                      let bytes = NSBitmapImageRep(cgImage: cgImage).representation(using: .png, properties: [:]), bytes.count <= 4 * 1024 * 1024 else {
                    completion(.failure(NativeBrowserFailure(message: "browser.screenshot_failed: capture failed or exceeded four MiB")))
                    return
                }
                completion(.success(.object(["data": .string(bytes.base64EncodedString()), "mime_type": "image/png", "width": .number(Double(cgImage.width)), "height": .number(Double(cgImage.height)), "viewport_width": .number(Double(viewport.width)), "viewport_height": .number(Double(viewport.height))])))
            }
        }
    }

    func webView(_ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction, decisionHandler: @escaping @MainActor @Sendable (WKNavigationActionPolicy) -> Void) {
        if navigationAction.targetFrame?.isMainFrame == true { console.navigation(to: navigationAction.request.url) }
        decisionHandler(Self.allowsNavigation(navigationAction.request.url) && navigationAllowed(navigationAction.request.url) ? .allow : .cancel)
    }

    private static func allowsNavigation(_ url: URL?) -> Bool {
        guard let url else { return false }
        let scheme = url.scheme?.lowercased() ?? ""
        return ["http", "https", "about", "blob"].contains(scheme)
    }

    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration, for navigationAction: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        guard webView === self.webView, navigationAllowed(navigationAction.request.url), navigationAction.request.url == nil || Self.allowsNavigation(navigationAction.request.url) else { return nil }
        return onPopup(configuration, navigationAction.request.url)
    }

    func webViewDidClose(_ webView: WKWebView) {
        guard webView === self.webView else { return }
        onClose()
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) { console.invalidate() }

    func webView(_ webView: WKWebView, didStartProvisionalNavigation navigation: WKNavigation!) { dialogs.cancel(); navigationEpoch += 1; queueState() }
    func webView(_ webView: WKWebView, didCommit navigation: WKNavigation!) { frames.removeAll(); popupRequestedURL = nil; navigationEpoch += 1; console.navigation(to: webView.url); queueState() }
    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) { queueState(); onNavigation() }
    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) { queueState() }
    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) { queueState() }
}

@MainActor
final class NativeBrowserInputBridge: NSObject, WKScriptMessageHandler {
    weak var page: NativeBrowserPage?

    func userContentController(_ userContentController: WKUserContentController, didReceive message: WKScriptMessage) {
        if message.name == "commissionFrame" { page?.registerFrame(message.body, info: message.frameInfo); return }
        page?.receiveLoopInput(message.body)
    }
}
