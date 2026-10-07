import AppKit
import WebKit
import Darwin

public typealias EngineEmitter = @MainActor (Int32, Int32, Data) -> Void

public enum EngineChannel {
    public static let wire: Int32 = 0
    public static let interface: Int32 = 1
}

@MainActor
public final class QareelEngine {
    nonisolated public static let abi: Int32 = 1
    nonisolated public static let version: StaticString = "0.2.0"
    static let operations = ["core", "snapshot", "evaluate", "reference_input", "screenshot", "automation", "console", "recording_video", "pointer_tap", "pointer_input", "dialog", "key_input", "wheel_input", "frames"]

    let profileID: UUID
    let store: WKWebsiteDataStore
    let recorder: NativeBrowserRecorder
    let instanceID = UUID().uuidString.lowercased()
    let emit: EngineEmitter
    let pageLimit: Int
    let presentation: String
    let extraOperations: [String]
    var popupsAllowed = true
    private var reserved: Set<String> = []
    private var scopes: [Int32: EngineScope] = [:]
    private var nextScope: Int32 = 0

    public init(profileID: UUID, store: WKWebsiteDataStore, recordings: URL, pageLimit: Int, presentation: String, extraOperations: [String], emit: @escaping EngineEmitter) {
        self.profileID = profileID
        self.store = store
        self.pageLimit = pageLimit
        self.presentation = presentation
        self.extraOperations = extraOperations.filter { !Self.operations.contains($0) && $0 != "popups" }
        self.emit = emit
        recorder = NativeBrowserRecorder(directory: recordings)
    }

    public func openScope(id: String, remote: Bool, recordingOwner: String?, popups: Bool) -> Int32 {
        nextScope += 1
        scopes[nextScope] = EngineScope(engine: self, handle: nextScope, id: id, remote: remote, recordingOwner: recordingOwner, popups: popups)
        return nextScope
    }

    public func submit(scope: Int32, data: Data) {
        scopes[scope]?.submit(data)
    }

    public var recording: Bool { recorder.recording }

    public func interruptAll(reason: String) {
        recorder.interrupt(reason: reason)
    }

    public func disconnect(scope: Int32) {
        scopes[scope]?.disconnect()
    }

    public func closeScope(_ scope: Int32) {
        scopes.removeValue(forKey: scope)?.stop()
    }

    public func view(scope: Int32, tab: String) -> NSView? {
        scopes[scope]?.page(tab)?.webView
    }

    public func query(scope: Int32, request: JSONValue) -> JSONValue {
        scopes[scope]?.query(request) ?? .null
    }

    public func control(scope: Int32, request: JSONValue) {
        if request["kind"]?.stringValue == "popups_allowed" {
            popupsAllowed = request["value"]?.boolValue ?? false
            return
        }
        scopes[scope]?.control(request)
    }

    func capabilities(popups: Bool) -> JSONValue {
        let operations = Self.operations + (popups ? ["popups"] : []) + extraOperations
        return .object(["version": .number(1), "implementation": .string("appkit_webkit"), "presentation": .string(presentation), "operations": .array(operations.map(JSONValue.string))])
    }

    func reserve(_ key: String) -> Bool {
        guard reserved.count < pageLimit, !reserved.contains(key) else { return false }
        reserved.insert(key)
        return true
    }

    func release(_ key: String) { reserved.remove(key) }

    func send(scope: Int32, channel: Int32, _ value: JSONValue) {
        emit(scope, channel, value.encoded())
    }

    static func allowsRemote(_ url: URL?) -> Bool {
        guard let url else { return true }
        if url.scheme == "about" { return true }
        if url.scheme == "blob" { return allowsRemote(URL(string: String(url.absoluteString.dropFirst(5)))) }
        guard ["http", "https"].contains(url.scheme?.lowercased() ?? ""), let raw = url.host?.lowercased() else { return false }
        let host = raw.trimmingCharacters(in: CharacterSet(charactersIn: "[]."))
        guard host != "localhost", !host.hasSuffix(".localhost"), !host.hasSuffix(".local") else { return false }
        var v4 = in_addr()
        if inet_aton(host, &v4) == 1 { let value = UInt32(bigEndian: v4.s_addr); return value >> 24 != 127 && value != 0 }
        var v6 = in6_addr()
        if inet_pton(AF_INET6, host, &v6) == 1 {
            let bytes = withUnsafeBytes(of: &v6) { Array($0) }
            if bytes.dropLast().allSatisfy({ $0 == 0 }), bytes.last == 0 || bytes.last == 1 { return false }
            if bytes.prefix(10).allSatisfy({ $0 == 0 }), bytes[10] == 255, bytes[11] == 255 { return bytes[12] != 127 && bytes.suffix(4).contains(where: { $0 != 0 }) }
        }
        return !host.contains("%")
    }
}

@MainActor
final class EngineScope {
    private static let commandLimit = 32
    private struct Popup {
        let id: String
        let openerID: String
        let sequence: UInt64
        var admitted: Bool
    }

    private unowned let engine: QareelEngine
    private let handle: Int32
    private let id: String
    private let remote: Bool
    private let recordingOwner: String?
    private let popupsEnabled: Bool
    private var generation: String?
    private var commands: [NativeBrowserCommand] = []
    private var worker: Task<Void, Never>?
    private var popupRetry: Task<Void, Never>?
    private var pages: [String: NativeBrowserPage] = [:]
    private var popups: [String: Popup] = [:]
    private var closedPopups: [String: Popup] = [:]
    private var popupSequence: UInt64 = 0
    private var inbound: [Data] = []
    private var reader: Task<Void, Never>?

    init(engine: QareelEngine, handle: Int32, id: String, remote: Bool, recordingOwner: String?, popups: Bool) {
        self.engine = engine
        self.handle = handle
        self.id = id
        self.remote = remote
        self.recordingOwner = recordingOwner
        popupsEnabled = popups
    }

    private var recorder: NativeBrowserRecorder { engine.recorder }
    private func key(_ tab: String) -> String { "\(id):\(tab)" }
    private func wire(_ value: JSONValue) { engine.send(scope: handle, channel: EngineChannel.wire, value) }
    private func interface(_ value: JSONValue) { engine.send(scope: handle, channel: EngineChannel.interface, value) }

    func page(_ tab: String) -> NativeBrowserPage? {
        guard let page = pages[tab], popups[tab]?.admitted != false else { return nil }
        return page
    }

    func submit(_ data: Data) {
        guard inbound.count < 64 else { return protocolFailure("browser.protocol_invalid: the host input queue is full") }
        inbound.append(data)
        guard reader == nil else { return }
        reader = Task { [weak self] in
            while let self, !self.inbound.isEmpty, !Task.isCancelled {
                let next = self.inbound.removeFirst()
                await self.receive(next)
            }
            self?.reader = nil
        }
    }

    private func receive(_ data: Data) async {
        guard let value = JSONValue.parse(data) else { return protocolFailure("browser.protocol_invalid: input is not JSON") }
        switch value["type"]?.stringValue {
        case "hello":
            guard let token = value["generation"]?.stringValue, !token.isEmpty, token.count <= 128 else { return protocolFailure("browser.protocol_invalid: invalid hello") }
            generation = token
            var ready: [String: JSONValue] = ["type": "ready", "generation": .string(token), "instance_id": .string(engine.instanceID)]
            if case .array(let features)? = value["features"], features.contains(where: { $0.stringValue == "ready-capabilities" }) {
                ready["capabilities"] = engine.capabilities(popups: popupsEnabled)
            }
            wire(.object(ready))
            for page in pages.values {
                page.hostGeneration = token
                if popups[page.id] == nil { publish(page) }
            }
            let replay = (Array(popups) + Array(closedPopups)).sorted { $0.value.sequence < $1.value.sequence }
            for (tabID, popup) in replay {
                if let page = pages[tabID] { publishPopup(page) } else { publishClosed(tabID: tabID, popup: popup) }
            }
            reconcilePopupRetry()
        case "command":
            guard let generation else { return protocolFailure("browser.protocol_invalid: command before hello") }
            let command: NativeBrowserCommand
            do { command = try JSONDecoder().decode(NativeBrowserCommand.self, from: data) }
            catch {
                guard value["generation"]?.stringValue == generation, let id = value["id"]?.stringValue, !id.isEmpty, id.utf8.count <= 128 else { return protocolFailure("browser.protocol_invalid: malformed command envelope") }
                let message = (error as? NativeBrowserFailure)?.message ?? "browser.protocol_invalid: malformed native command"
                wire(.object(["type": "reply", "generation": .string(generation), "id": .string(id), "outcome": .object(["kind": "error", "message": .string(message)])]))
                return
            }
            guard command.generation == generation, command.id.utf8.count <= 128, command.tab_id.utf8.count <= 128 else { return protocolFailure("browser.stale_host: command belongs to another connection") }
            if command.operation.direct {
                do { reply(command, outcome: .object(["kind": "ok", "value": try await execute(command)])) }
                catch { reply(command, outcome: .object(["kind": "error", "message": .string(error.localizedDescription)])) }
            } else if commands.count >= Self.commandLimit {
                reply(command, outcome: .object(["kind": "error", "message": "browser.busy: native command queue is full"]))
            } else {
                commands.append(command)
                drainCommands()
            }
        default:
            protocolFailure("browser.protocol_invalid: unknown message type")
        }
    }

    private func protocolFailure(_ reason: String) {
        interface(.object(["type": "protocol_failure", "reason": .string(reason)]))
    }

    private func drainCommands() {
        guard worker == nil else { return }
        worker = Task { [weak self] in
            guard let self else { return }
            defer { worker = nil }
            while !commands.isEmpty, !Task.isCancelled {
                let command = commands.removeFirst()
                guard command.generation == generation else { continue }
                do {
                    let value = try await execute(command)
                    reply(command, outcome: .object(["kind": "ok", "value": value]))
                } catch {
                    if error.localizedDescription.contains("browser.process_unresponsive"), let page = pages[command.tab_id] {
                        if popups[page.id] != nil { closePopup(page) } else { removePage(page) }
                    }
                    reply(command, outcome: .object(["kind": "error", "message": .string(error.localizedDescription)]))
                }
            }
        }
    }

    private func execute(_ command: NativeBrowserCommand) async throws -> JSONValue {
        guard command.generation == generation, Date().timeIntervalSince1970 * 1000 <= Double(command.deadline_ms) else { throw NativeBrowserFailure(message: "browser.command_expired: command belongs to an expired connection or deadline") }
        if case .popupDecision(let instance, let sequence, let popupID, let openerID, let decision) = command.operation {
            guard !command.agent else { throw NativeBrowserFailure(message: "browser.control_denied: popup registration is daemon-owned") }
            if instance == engine.instanceID, decision != .admit, pages[command.tab_id] == nil, popups[command.tab_id] == nil, closedPopups[command.tab_id] == nil { return .null }
            guard instance == engine.instanceID, let popup = popups[command.tab_id] ?? closedPopups[command.tab_id], popup.id == popupID, popup.openerID == openerID, popup.sequence == sequence else { throw NativeBrowserFailure(message: "browser.popup_stale: popup identity no longer exists") }
            switch decision {
            case .admit:
                guard var current = popups[command.tab_id], let page = pages[command.tab_id] else {
                    publishClosed(tabID: command.tab_id, popup: popup)
                    throw NativeBrowserFailure(message: "browser.popup_closed: popup closed before admission")
                }
                current.admitted = true
                popups[command.tab_id] = current
                reconcilePopupRetry()
                interface(.object(["type": "page_changed", "tab": .string(page.id)]))
                publish(page)
                return page.state
            case .reject, .closed:
                if decision == .reject, popups[command.tab_id]?.admitted == true { throw NativeBrowserFailure(message: "browser.popup_stale: an admitted popup cannot be rejected") }
                if let page = pages[command.tab_id] { removePage(page) }
                popups.removeValue(forKey: command.tab_id)
                closedPopups.removeValue(forKey: command.tab_id)
                reconcilePopupRetry()
                return .null
            }
        }
        if case .cookiesExport = command.operation { throw NativeBrowserFailure(message: "browser.capability_unavailable: cookie sync belongs to the embedding app") }
        if case .cookiesApply = command.operation { throw NativeBrowserFailure(message: "browser.capability_unavailable: cookie sync belongs to the embedding app") }
        if case .recording(let kind, let value) = command.operation {
            guard let recordingID = value["recording_id"]?.stringValue else { throw NativeBrowserFailure(message: "browser.recording_id_invalid") }
            switch kind {
            case "recording_start":
                guard let encoded = value["options"], let page = pages[command.tab_id], popups[page.id]?.admitted != false else { throw NativeBrowserFailure(message: "browser.tab_unavailable") }
                try page.validate(agent: command.agent, epoch: command.control_epoch, deadline: Double(command.deadline_ms), generation: command.generation)
                let options = try JSONDecoder().decode(NativeRecordingOptions.self, from: encoded.encoded())
                return try await recorder.start(id: recordingID, options: options, page: page, agent: command.agent, epoch: command.control_epoch, deadline: Double(command.deadline_ms), generation: command.generation, owner: recordingOwner)
            case "recording_caption":
                if command.agent {
                    guard let page = pages[command.tab_id] else { throw NativeBrowserFailure(message: "browser.tab_unavailable") }
                    try page.validate(agent: true, epoch: command.control_epoch, deadline: Double(command.deadline_ms), generation: command.generation)
                }
                guard let captionID = value["caption_id"]?.stringValue, UUID(uuidString: captionID) != nil, let text = value["text"]?.stringValue else { throw NativeBrowserFailure(message: "browser.recording_caption_invalid") }
                return try await recorder.caption(id: recordingID, tab: command.tab_id, captionID: captionID, text: text, owner: recordingOwner)
            case "recording_stop": return try await recorder.stop(id: recordingID, tab: command.tab_id, owner: recordingOwner)
            case "recording_status": return try await recorder.status(id: recordingID, tab: command.tab_id, owner: recordingOwner)
            case "recording_read":
                guard let artifact = value["artifact"]?.stringValue, let offset = value["offset"]?.doubleValue, offset >= 0, offset.rounded() == offset, offset <= 536870912, let maximum = value["max_bytes"]?.doubleValue, maximum > 0, maximum <= 262144, maximum.rounded() == maximum else { throw NativeBrowserFailure(message: "browser.recording_read_invalid") }
                return try await recorder.storage.read(id: recordingID, tab: command.tab_id, kind: artifact, offset: UInt64(offset), maximum: Int(maximum), owner: recordingOwner)
            case "recording_release":
                guard let value = value["artifacts"] else { throw NativeBrowserFailure(message: "browser.recording_release_invalid") }
                let artifacts = try JSONDecoder().decode([NativeRecordingArtifact].self, from: value.encoded())
                try await recorder.storage.release(id: recordingID, tab: command.tab_id, artifacts: artifacts, owner: recordingOwner)
                return .null
            default: throw NativeBrowserFailure(message: "browser.recording_operation_invalid")
            }
        }
        if case .ensure(let workspace, let profile, let url) = command.operation {
            guard closedPopups[command.tab_id] == nil else { throw NativeBrowserFailure(message: "browser.popup_closed: popup is waiting for closure acknowledgement") }
            if let existing = pages[command.tab_id] {
                guard existing.workspaceID == workspace, existing.profileID == profile else { throw NativeBrowserFailure(message: "browser.tab_scope_mismatch") }
                guard popups[existing.id]?.admitted != false else { throw NativeBrowserFailure(message: "browser.popup_pending: popup registration is not complete") }
                return existing.state
            }
            guard engine.reserve(key(command.tab_id)) else { throw NativeBrowserFailure(message: "browser.tab_limit: native page capacity is reserved or full") }
            let page = NativeBrowserPage(id: command.tab_id, workspaceID: workspace, profileID: profile, store: engine.store)
            configure(page)
            do { try page.navigate(url) }
            catch {
                engine.release(key(command.tab_id))
                page.disconnect()
                page.renderWindow.close()
                throw error
            }
            pages[page.id] = page
            interface(.object(["type": "page_added", "tab": .string(page.id)]))
            return page.state
        }
        guard let page = pages[command.tab_id] else { throw NativeBrowserFailure(message: "browser.tab_unavailable: restore this tab before using it") }
        guard popups[page.id]?.admitted != false else { throw NativeBrowserFailure(message: "browser.popup_pending: popup registration is not complete") }
        if case .dialog(let action) = command.operation {
            guard command.agent else { throw NativeBrowserFailure(message: "browser.control_denied: the user answers dialogs in the browser window") }
            try page.validate(agent: true, epoch: command.control_epoch, deadline: Double(command.deadline_ms), generation: command.generation, allowingDialog: true)
            return try page.dialogs.perform(action)
        }
        try page.validate(agent: command.agent, epoch: command.control_epoch, deadline: Double(command.deadline_ms), generation: command.generation)
        if !command.agent, command.operation.manualMutation { page.takeover() }
        page.beginActivity()
        defer {
            page.endActivity()
            if command.agent { page.hold(milliseconds: 30000) }
        }
        switch command.operation {
        case .ensure, .popupDecision, .recording, .dialog, .cookiesExport, .cookiesApply: break
        case .navigate(let url): try page.navigate(url)
        case .back: page.webView.goBack()
        case .forward: page.webView.goForward()
        case .reload: page.webView.reload()
        case .close:
            removePage(page)
            popups.removeValue(forKey: page.id)
            reconcilePopupRetry()
        case .resize(let width, let height):
            try page.resize(width: width, height: height)
            interface(.object(["type": "page_changed", "tab": .string(page.id)]))
        case .automationObserve(let bootstrap):
            guard command.agent else { throw NativeBrowserFailure(message: "browser.control_denied: runner operations require an agent command") }
            return try await page.automation.observe(bootstrap: bootstrap, epoch: command.control_epoch, deadline: Double(command.deadline_ms), generation: command.generation)
        case .automationExecute(let binding, let token, let action):
            guard command.agent else { throw NativeBrowserFailure(message: "browser.control_denied: runner operations require an agent command") }
            try await page.automation.execute(binding: binding, token: token, action: action, epoch: command.control_epoch, deadline: Double(command.deadline_ms), generation: command.generation)
        case .pointer(let phase, let x, let y):
            if phase == "down" { try await markTarget(page, x: x, y: y, command: command) }
            if phase == "move" && !page.pointerHeld { try page.pointer(.object(["x": .number(x), "y": .number(y)]), click: false) }
            else { try page.heldPointer(phase: phase, x: x, y: y) }
        case .tap(let x, let y):
            try await markTarget(page, x: x, y: y, command: command)
            try page.pointer(.object(["x": .number(x), "y": .number(y)]), click: true)
        case .hold(let ms, let input): page.hold(milliseconds: ms, input: input)
        case .key(let phase, let key): try page.trustedKey(phase: phase, name: key)
        case .wheel(let x, let y, let dx, let dy, let zoom): try page.wheel(x: x, y: y, dx: dx, dy: dy, zoom: zoom)
        case .frames: return page.frameList()
        case .frameEvaluate(let frame, let script): return try await page.evaluate(script, frame: frame)
        case .snapshot: return try await page.evaluate(NativeBrowserScript.snapshot, isolated: true)
        case .console(let action): return try await page.console.perform(action)
        case .evaluate(let script): return try await page.evaluate(script)
        case .screenshot: return try await page.screenshot()
        case .click(let reference): try await page.click(reference: reference, agent: command.agent, epoch: command.control_epoch, deadline: Double(command.deadline_ms), generation: command.generation)
        case .type(let text, let reference): try await page.type(text: text, reference: reference, agent: command.agent, epoch: command.control_epoch, deadline: Double(command.deadline_ms), generation: command.generation)
        case .press(let key): try page.press(key)
        case .control(let human):
            guard !command.agent else { throw NativeBrowserFailure(message: "browser.control_denied: only the user may resume automation") }
            try page.control(epoch: command.control_epoch, human: human)
        }
        publish(page)
        return page.state
    }

    private static func hitTest(x: Double, y: Double) -> String {
        let point = String(decoding: JSONValue.object(["x": .number(x), "y": .number(y)]).encoded(), as: UTF8.self)
        return """
        (() => {
          const point = \(point);
          const node = document.elementFromPoint(point.x, point.y);
          if (!node || node === document.documentElement || node === document.body) return null;
          const rect = node.getBoundingClientRect();
          const left = Math.max(0, rect.left), top = Math.max(0, rect.top);
          const right = Math.min(innerWidth, rect.right), bottom = Math.min(innerHeight, rect.bottom);
          if (!(right > left && bottom > top)) return null;
          return {x: point.x, y: point.y, bounds: {left, top, width: right - left, height: bottom - top}};
        })()
        """
    }

    private func markTarget(_ page: NativeBrowserPage, x: Double, y: Double, command: NativeBrowserCommand) async throws {
        let navigation = page.navigationEpoch
        let target = try? await page.evaluate(Self.hitTest(x: x, y: y), isolated: true)
        try page.validate(agent: command.agent, epoch: command.control_epoch, deadline: Double(command.deadline_ms), generation: command.generation)
        guard let target, target["bounds"] != nil, navigation == page.navigationEpoch else { return }
        page.recordAutomationTarget(target)
    }

    private func reply(_ command: NativeBrowserCommand, outcome: JSONValue) {
        guard generation == command.generation else { return }
        wire(.object(["type": "reply", "generation": .string(command.generation), "id": .string(command.id), "outcome": outcome]))
    }

    private func publish(_ page: NativeBrowserPage) {
        guard let generation, pages[page.id] === page else { return }
        if popups[page.id]?.admitted == false { publishPopup(page); return }
        wire(.object(["type": "tab", "generation": .string(generation), "tab_id": .string(page.id), "state": page.state]))
    }

    private func configure(_ page: NativeBrowserPage) {
        page.navigationAllowed = { [remote] url in !remote || QareelEngine.allowsRemote(url) }
        page.onAutomationTarget = { [weak self, weak page] rect, navigation in if let self, let page { recorder.target(page: page, rect: rect, navigation: navigation) } }
        page.onRecordingPointer = { [weak self, weak page] point, click in
            guard let self, let page else { return }
            recorder.pointer(page: page, point: point, click: click)
            interface(.object(["type": "pointer", "tab": .string(page.id), "x": .number(point.x), "y": .number(point.y), "click": .bool(click), "human": .bool(page.human)]))
        }
        page.onNavigation = { [weak self, weak page] in if let self, let page { interface(.object(["type": "navigation", "tab": .string(page.id)])) } }
        page.hostGeneration = generation
        page.onState = { [weak self, weak page] in
            guard let self, let page else { return }
            publish(page)
            interface(.object(["type": "page_changed", "tab": .string(page.id)]))
        }
        page.onTakeover = { [weak self, weak page] epoch in
            guard let self, let page, pages[page.id] === page, popups[page.id]?.admitted != false, let generation else { return }
            wire(.object(["type": "takeover", "generation": .string(generation), "tab_id": .string(page.id), "control_epoch": .number(Double(epoch))]))
        }
        page.onPopup = { [weak self, weak page] configuration, requestedURL in
            guard let self, let page else { return nil }
            return makePopup(opener: page, configuration: configuration, requestedURL: requestedURL)
        }
        page.onClose = { [weak self, weak page] in
            guard let self, let page else { return }
            closePopup(page)
        }
    }

    private func makePopup(opener: NativeBrowserPage, configuration: WKWebViewConfiguration, requestedURL: URL?) -> WKWebView? {
        guard popupsEnabled, engine.popupsAllowed, generation != nil, pages[opener.id] === opener, popups[opener.id]?.admitted != false,
              popups.values.filter({ !$0.admitted }).count < 4, closedPopups.count + popups.count < 64, popupSequence < 9_007_199_254_740_991,
              configuration.websiteDataStore === opener.webView.configuration.websiteDataStore,
              opener.navigationAllowed(requestedURL) else { return nil }
        let tabID = UUID().uuidString.lowercased()
        guard engine.reserve(key(tabID)) else { return nil }
        let page = NativeBrowserPage(id: tabID, workspaceID: opener.workspaceID, profileID: opener.profileID, store: engine.store, suppliedConfiguration: configuration, popupRequestedURL: requestedURL)
        configure(page)
        if opener.human { page.takeover() }
        popupSequence += 1
        popups[tabID] = Popup(id: UUID().uuidString.lowercased(), openerID: opener.id, sequence: popupSequence, admitted: false)
        pages[tabID] = page
        interface(.object(["type": "page_added", "tab": .string(tabID)]))
        publishPopup(page)
        reconcilePopupRetry()
        return page.webView
    }

    private func removePage(_ page: NativeBrowserPage) {
        recorder.interrupt(page: page, reason: "Recorded tab closed")
        guard pages[page.id] === page else { return }
        pages.removeValue(forKey: page.id)
        page.onPopup = { _, _ in nil }
        page.onClose = {}
        page.dialogs.invalidate()
        page.disconnect()
        page.webView.stopLoading()
        page.webView.removeFromSuperview()
        page.renderWindow.close()
        engine.release(key(page.id))
        interface(.object(["type": "page_removed", "tab": .string(page.id)]))
    }

    private func closePopup(_ page: NativeBrowserPage) {
        guard pages[page.id] === page, let popup = popups.removeValue(forKey: page.id) else { return }
        closedPopups[page.id] = popup
        removePage(page)
        publishClosed(tabID: page.id, popup: popup)
        reconcilePopupRetry()
    }

    private func publishPopup(_ page: NativeBrowserPage) {
        guard let generation, pages[page.id] === page, let popup = popups[page.id] else { return }
        wire(.object(["type": "popup", "generation": .string(generation), "instance_id": .string(engine.instanceID), "sequence": .number(Double(popup.sequence)), "opener_id": .string(popup.openerID), "tab_id": .string(page.id), "popup_id": .string(popup.id), "state": page.state]))
    }

    private func publishClosed(tabID: String, popup: Popup) {
        guard let generation else { return }
        wire(.object(["type": "popup_closed", "generation": .string(generation), "instance_id": .string(engine.instanceID), "sequence": .number(Double(popup.sequence)), "opener_id": .string(popup.openerID), "tab_id": .string(tabID), "popup_id": .string(popup.id)]))
    }

    private func reconcilePopupRetry() {
        guard generation != nil, !closedPopups.isEmpty || popups.values.contains(where: { !$0.admitted }) else {
            popupRetry?.cancel()
            popupRetry = nil
            return
        }
        guard popupRetry == nil else { return }
        popupRetry = Task { [weak self] in
            while !Task.isCancelled {
                do { try await Task.sleep(for: .seconds(2)) } catch { return }
                guard let self, generation != nil else { return }
                let pending = popups.filter { !$0.value.admitted }
                let replay = (Array(pending) + Array(closedPopups)).sorted { $0.value.sequence < $1.value.sequence }
                for (tabID, popup) in replay {
                    if let page = pages[tabID] { publishPopup(page) } else { publishClosed(tabID: tabID, popup: popup) }
                }
            }
        }
    }

    func query(_ request: JSONValue) -> JSONValue {
        switch request["kind"]?.stringValue {
        case "page":
            guard let tab = request["tab"]?.stringValue, let page = pages[tab] else { return .null }
            let viewport = page.viewportOverride.map { JSONValue.array([.number($0.width), .number($0.height)]) } ?? .null
            return .object(["tab": .string(page.id), "workspace_id": .string(page.workspaceID), "admitted": .bool(popups[page.id]?.admitted != false), "viewport": viewport, "page_zoom": .number(page.webView.pageZoom), "human": .bool(page.human), "automating": .bool(page.webView.automationDepth > 0), "state": page.state])
        case "pages":
            return .array(pages.keys.sorted().map(JSONValue.string))
        default:
            return .null
        }
    }

    func control(_ request: JSONValue) {
        guard let tab = request["tab"]?.stringValue, let page = pages[tab] else { return }
        switch request["kind"]?.stringValue {
        case "hide": page.hide()
        case "present_dialogs": page.dialogs.presentIfVisible()
        case "human_pointer":
            guard page.webView.automationDepth == 0, let x = request["x"]?.doubleValue, let y = request["y"]?.doubleValue, x.isFinite, y.isFinite else { return }
            recorder.pointer(page: page, point: CGPoint(x: x, y: y), click: request["click"]?.boolValue ?? false)
        default: return
        }
    }

    func disconnect() {
        for page in pages.values { recorder.interrupt(page: page, reason: "Native host disconnected") }
        generation = nil
        reconcilePopupRetry()
        commands.removeAll()
        for page in pages.values { page.disconnect() }
    }

    func stop() {
        disconnect()
        for page in Array(pages.values) { removePage(page) }
        popups.removeAll()
        closedPopups.removeAll()
        worker?.cancel()
        popupRetry?.cancel()
        reader?.cancel()
        inbound.removeAll()
    }
}
