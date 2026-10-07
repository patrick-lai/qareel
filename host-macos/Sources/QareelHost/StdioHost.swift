import AppKit
import WebKit
import Darwin

private struct NativeBrowserCommand: Decodable {
    let generation: String
    let id: String
    let tab_id: String
    let agent: Bool
    let control_epoch: UInt64
    let deadline_ms: UInt64
    let operation: Operation

    enum Operation: Decodable {
        case ensure(workspace: String, profile: UUID, url: String)
        case navigate(String)
        case back, forward, reload, close, snapshot, screenshot
        case evaluate(String)
        case click(String)
        case type(String, String?)
        case press(String)
        case automationObserve(String)
        case automationExecute(NativeBrowserAutomationBinding, String, NativeBrowserAutomationAction)
        case resize(Int?, Int?)
        case recording(String, JSONValue)
        case console(NativeBrowserConsoleAction)
        case pointer(String, Double, Double)
        case tap(Double, Double)
        case hold(Int, String?)
        case key(String, String)
        case wheel(Double, Double, Double, Double, Bool)
        case frames
        case frameEvaluate(String, String)
        case dialog(JSONValue)

        private enum Keys: String, CodingKey { case binding, action, width, height }

        init(from decoder: Decoder) throws {
            let value = try JSONValue(from: decoder)
            func required(_ key: String, limit: Int = 65536) throws -> String {
                guard let text = value[key]?.stringValue, text.utf8.count <= limit else { throw NativeBrowserFailure(message: "browser.protocol_invalid: missing or oversized \(key)") }
                return text
            }
            switch value["kind"]?.stringValue {
            case "ensure":
                guard let profile = UUID(uuidString: try required("profile_id", limit: 36)) else { throw NativeBrowserFailure(message: "browser.profile_invalid: expected a profile identifier") }
                self = .ensure(workspace: try required("workspace_id", limit: 128), profile: profile, url: try required("url", limit: 16384))
            case "navigate": self = .navigate(try required("url", limit: 16384))
            case "back": self = .back
            case "forward": self = .forward
            case "reload": self = .reload
            case "close": self = .close
            case "snapshot": self = .snapshot
            case "screenshot": self = .screenshot
            case "evaluate": self = .evaluate(try required("script", limit: 131072))
            case "click": self = .click(try required("reference", limit: 256))
            case "type": self = .type(try required("text"), value["reference"]?.stringValue)
            case "press": self = .press(try required("key", limit: 128))
            case "automation_observe": self = .automationObserve(try required("bootstrap"))
            case "console":
                let container = try decoder.container(keyedBy: Keys.self)
                self = .console(try container.decode(NativeBrowserConsoleAction.self, forKey: .action))
            case "automation_execute":
                let container = try decoder.container(keyedBy: Keys.self)
                self = .automationExecute(try container.decode(NativeBrowserAutomationBinding.self, forKey: .binding), try required("token", limit: 256), try container.decode(NativeBrowserAutomationAction.self, forKey: .action))
            case "resize":
                let container = try decoder.container(keyedBy: Keys.self)
                self = .resize(try container.decodeIfPresent(Int.self, forKey: .width), try container.decodeIfPresent(Int.self, forKey: .height))
            case "recording_start", "recording_caption", "recording_stop", "recording_status", "recording_read", "recording_release":
                guard let kind = value["kind"]?.stringValue, let id = value["recording_id"]?.stringValue, UUID(uuidString: id)?.uuidString.lowercased() == id else { throw NativeBrowserFailure(message: "browser.recording_id_invalid") }
                self = .recording(kind, value)
            case "pointer":
                guard let phase = value["phase"]?.stringValue, ["move", "down", "up"].contains(phase), let x = value["x"]?.doubleValue, let y = value["y"]?.doubleValue, x.isFinite, y.isFinite else { throw NativeBrowserFailure(message: "browser.protocol_invalid: invalid pointer input") }
                self = .pointer(phase, x, y)
            case "tap":
                guard let x = value["x"]?.doubleValue, let y = value["y"]?.doubleValue, x.isFinite, y.isFinite else { throw NativeBrowserFailure(message: "browser.protocol_invalid: invalid tap position") }
                self = .tap(x, y)
            case "hold":
                guard let ms = value["ms"]?.doubleValue, ms >= 0, ms <= 620000 else { throw NativeBrowserFailure(message: "browser.protocol_invalid: invalid hold duration") }
                self = .hold(Int(ms), value["input"]?.stringValue)
            case "wheel":
                guard let x = value["x"]?.doubleValue, let y = value["y"]?.doubleValue, let dx = value["dx"]?.doubleValue, let dy = value["dy"]?.doubleValue, [x, y, dx, dy].allSatisfy(\.isFinite), abs(dx) <= 20000, abs(dy) <= 20000 else { throw NativeBrowserFailure(message: "browser.protocol_invalid: invalid wheel input") }
                self = .wheel(x, y, dx, dy, value["zoom"]?.boolValue ?? false)
            case "frames": self = .frames
            case "frame_evaluate":
                guard let frame = value["frame"]?.stringValue, !frame.isEmpty, frame.count <= 2048 else { throw NativeBrowserFailure(message: "browser.protocol_invalid: invalid frame") }
                self = .frameEvaluate(frame, try required("script", limit: 4 * 1024 * 1024))
            case "key":
                guard let phase = value["phase"]?.stringValue, ["down", "up"].contains(phase), let key = value["key"]?.stringValue, !key.isEmpty, key.count <= 32 else { throw NativeBrowserFailure(message: "browser.protocol_invalid: invalid key") }
                self = .key(phase, key)
            case "dialog":
                guard let action = value["action"] else { throw NativeBrowserFailure(message: "browser.protocol_invalid: missing dialog action") }
                self = .dialog(action)
            default: throw NativeBrowserFailure(message: "browser.capability_unavailable: unknown native operation")
            }
        }

        var direct: Bool {
            switch self { case .recording: true; default: false }
        }
    }
}

struct QareelBootstrap: Sendable {
    let directory: URL
    let profileID: UUID

    static func read() throws -> QareelBootstrap {
        var line = Data()
        var byte: UInt8 = 0
        while true {
            let count = Darwin.read(STDIN_FILENO, &byte, 1)
            if count < 0 {
                if errno == EINTR { continue }
                throw NativeBrowserFailure(message: "browser.configuration_invalid: cannot read the bootstrap line (errno \(errno))")
            }
            if count == 0 {
                guard !line.isEmpty else { throw NativeBrowserFailure(message: "browser.configuration_invalid: stdin closed before the bootstrap line") }
                break
            }
            if byte == 10 { break }
            line.append(byte)
            guard line.count <= 65536 else { throw NativeBrowserFailure(message: "browser.configuration_invalid: bootstrap line exceeds 64 KiB") }
        }
        return try parse(line)
    }

    static func parse(_ data: Data) throws -> QareelBootstrap {
        guard case .object(let fields)? = JSONValue.parse(data) else { throw NativeBrowserFailure(message: "browser.configuration_invalid: bootstrap must be one JSON object") }
        for (key, value) in fields {
            switch key {
            case "profile_dir", "profile_id": continue
            case "ffmpeg", "pulseaudio", "dbus_daemon":
                guard value == .null || value.stringValue != nil else { throw NativeBrowserFailure(message: "browser.configuration_invalid: \(key) must be a path string") }
            default: throw NativeBrowserFailure(message: "browser.configuration_invalid: unknown bootstrap key \(String(key.prefix(64)))")
            }
        }
        guard let path = fields["profile_dir"]?.stringValue, path.hasPrefix("/"), path.utf8.count <= 1024, !path.contains("\0") else { throw NativeBrowserFailure(message: "browser.configuration_invalid: profile_dir must be an absolute path") }
        guard let text = fields["profile_id"]?.stringValue, let profile = UUID(uuidString: text) else { throw NativeBrowserFailure(message: "browser.configuration_invalid: profile_id must be a UUID") }
        return QareelBootstrap(directory: URL(fileURLWithPath: path, isDirectory: true).standardizedFileURL, profileID: profile)
    }

    func lock() throws -> Int32 {
        do { try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700]) }
        catch { throw NativeBrowserFailure(message: "browser.configuration_invalid: cannot create profile_dir \(directory.path): \(error.localizedDescription)") }
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: directory.path, isDirectory: &isDirectory), isDirectory.boolValue else { throw NativeBrowserFailure(message: "browser.configuration_invalid: profile_dir \(directory.path) is not a directory") }
        let fd = open(directory.appendingPathComponent(".qareel-host.lock").path, O_CREAT | O_RDWR | O_NOFOLLOW | O_CLOEXEC, mode_t(0o600))
        guard fd >= 0 else { throw NativeBrowserFailure(message: "browser.configuration_invalid: cannot open the profile lock in \(directory.path) (errno \(errno))") }
        guard flock(fd, LOCK_EX | LOCK_NB) == 0 else {
            let code = errno
            _ = Darwin.close(fd)
            if code == EWOULDBLOCK { throw NativeBrowserFailure(message: "browser.profile_locked: another qareel host is using \(directory.path)") }
            throw NativeBrowserFailure(message: "browser.configuration_invalid: cannot lock \(directory.path) (errno \(code))")
        }
        return fd
    }
}

enum StdioInput: Sendable {
    case line(Data)
    case end
    case oversized
    case failed(Int32)
}

enum StdioReader {
    static let lineLimit = 8 * 1024 * 1024

    static func start(_ continuation: AsyncStream<StdioInput>.Continuation, permits: DispatchSemaphore) {
        let thread = Thread {
            var chunk = [UInt8](repeating: 0, count: 65536)
            var line = Data()
            while true {
                let count = chunk.withUnsafeMutableBytes { Darwin.read(STDIN_FILENO, $0.baseAddress, $0.count) }
                if count < 0 {
                    let code = errno
                    if code == EINTR { continue }
                    continuation.yield(.failed(code))
                    continuation.finish()
                    return
                }
                if count == 0 {
                    if !line.isEmpty { permits.wait(); continuation.yield(.line(line)) }
                    continuation.yield(.end)
                    continuation.finish()
                    return
                }
                var start = 0
                while let index = chunk[start..<count].firstIndex(of: 10) {
                    line.append(contentsOf: chunk[start..<index])
                    guard line.count <= lineLimit else { continuation.yield(.oversized); continuation.finish(); return }
                    if !line.isEmpty { permits.wait(); continuation.yield(.line(line)) }
                    line = Data()
                    start = index + 1
                }
                line.append(contentsOf: chunk[start..<count])
                guard line.count <= lineLimit else { continuation.yield(.oversized); continuation.finish(); return }
            }
        }
        thread.name = "qareel-host.stdin"
        thread.start()
    }
}

enum StdioOutput {
    static func write(_ data: Data) -> Bool {
        data.withUnsafeBytes { buffer in
            guard let base = buffer.baseAddress else { return true }
            var offset = 0
            while offset < buffer.count {
                let count = Darwin.write(STDOUT_FILENO, base.advanced(by: offset), buffer.count - offset)
                if count > 0 { offset += count; continue }
                let code = errno
                if count < 0 && code == EINTR { continue }
                if count < 0 && code == EAGAIN {
                    var descriptor = pollfd(fd: STDOUT_FILENO, events: Int16(POLLOUT), revents: 0)
                    _ = poll(&descriptor, 1, -1)
                    continue
                }
                return false
            }
            return true
        }
    }

    static func log(_ text: String) {
        _ = try? FileHandle.standardError.write(contentsOf: Data((text + "\n").utf8))
    }
}

@MainActor
final class StdioHost {
    private static let pageLimit = 8
    private static let commandLimit = 32
    private static let shutdownGrace = 8.0
    private static let capabilities: JSONValue = .object([
        "version": .number(1),
        "implementation": .string("appkit_webkit"),
        "presentation": .string("none"),
        "operations": .array(["core", "snapshot", "evaluate", "reference_input", "screenshot", "automation", "console", "recording_video", "pointer_tap", "pointer_input", "dialog", "key_input", "wheel_input", "frames"].map(JSONValue.string))
    ])

    private let profileID: UUID
    private let store: WKWebsiteDataStore
    private let recorder: NativeBrowserRecorder
    private let lockFD: Int32
    private let instanceID = UUID().uuidString.lowercased()
    private var generation: String?
    private var commands: [NativeBrowserCommand] = []
    private var worker: Task<Void, Never>?
    private var pages: [String: NativeBrowserPage] = [:]
    private var recording: (id: String, tab: String)?
    private var stopping = false
    private var signalSources: [any DispatchSourceSignal] = []
    private var activity: (any NSObjectProtocol)?

    init(bootstrap: QareelBootstrap, lockFD: Int32) {
        profileID = bootstrap.profileID
        store = WKWebsiteDataStore(forIdentifier: bootstrap.profileID)
        recorder = NativeBrowserRecorder(directory: bootstrap.directory.appendingPathComponent("recordings", isDirectory: true))
        self.lockFD = lockFD
    }

    func start() {
        activity = ProcessInfo.processInfo.beginActivity(options: [.userInitiatedAllowingIdleSystemSleep], reason: "qareel browser host")
        for code in [SIGTERM, SIGINT] {
            signal(code, SIG_IGN)
            let source = DispatchSource.makeSignalSource(signal: code, queue: .main)
            source.setEventHandler { [weak self] in
                guard let self else { return }
                MainActor.assumeIsolated { self.shutdown(code: 0, reason: nil) }
            }
            source.resume()
            signalSources.append(source)
        }
        let (stream, continuation) = AsyncStream<StdioInput>.makeStream()
        let permits = DispatchSemaphore(value: 16)
        StdioReader.start(continuation, permits: permits)
        Task { [weak self] in
            for await input in stream {
                guard let self else { return }
                await receive(input)
                permits.signal()
            }
        }
    }

    private func receive(_ input: StdioInput) async {
        guard !stopping else { return }
        switch input {
        case .end: shutdown(code: 0, reason: nil)
        case .oversized: shutdown(code: 1, reason: "browser.protocol_invalid: input line exceeds 8 MiB")
        case .failed(let code): shutdown(code: 1, reason: "browser.transport_failed: stdin read failed (errno \(code))")
        case .line(let data): await receive(line: data)
        }
    }

    private func receive(line data: Data) async {
        guard let value = JSONValue.parse(data) else { return shutdown(code: 1, reason: "browser.protocol_invalid: input line is not JSON") }
        switch value["type"]?.stringValue {
        case "hello":
            guard generation == nil, let token = value["generation"]?.stringValue, !token.isEmpty, token.count <= 128 else { return shutdown(code: 1, reason: "browser.protocol_invalid: unexpected or invalid hello") }
            generation = token
            var ready: [String: JSONValue] = ["type": "ready", "generation": .string(token), "instance_id": .string(instanceID)]
            if case .array(let features)? = value["features"], features.contains(where: { $0.stringValue == "ready-capabilities" }) {
                ready["capabilities"] = Self.capabilities
            }
            send(.object(ready))
        case "command":
            guard let generation else { return shutdown(code: 1, reason: "browser.protocol_invalid: command before hello") }
            let command: NativeBrowserCommand
            do { command = try JSONDecoder().decode(NativeBrowserCommand.self, from: data) }
            catch {
                guard value["generation"]?.stringValue == generation, let id = value["id"]?.stringValue, !id.isEmpty, id.utf8.count <= 128 else { return shutdown(code: 1, reason: "browser.protocol_invalid: malformed command envelope") }
                let message = (error as? NativeBrowserFailure)?.message ?? "browser.protocol_invalid: malformed native command"
                send(.object(["type": "reply", "generation": .string(generation), "id": .string(id), "outcome": .object(["kind": "error", "message": .string(message)])]))
                return
            }
            guard command.generation == generation, command.id.utf8.count <= 128, command.tab_id.utf8.count <= 128 else { return shutdown(code: 1, reason: "browser.stale_host: command belongs to another connection") }
            if command.operation.direct {
                do {
                    let result = try await execute(command)
                    reply(command, outcome: .object(["kind": "ok", "value": result]))
                } catch {
                    reply(command, outcome: .object(["kind": "error", "message": .string(error.localizedDescription)]))
                }
            } else if commands.count >= Self.commandLimit {
                reply(command, outcome: .object(["kind": "error", "message": "browser.busy: native command queue is full"]))
            } else {
                commands.append(command)
                drainCommands()
            }
        default:
            shutdown(code: 1, reason: "browser.protocol_invalid: unknown message type")
        }
    }

    private func drainCommands() {
        guard worker == nil else { return }
        worker = Task { [weak self] in
            guard let self else { return }
            defer { worker = nil }
            while !commands.isEmpty, !Task.isCancelled, !stopping {
                let command = commands.removeFirst()
                guard command.generation == generation else { continue }
                do {
                    let value = try await execute(command)
                    reply(command, outcome: .object(["kind": "ok", "value": value]))
                } catch {
                    if error.localizedDescription.contains("browser.process_unresponsive"), let page = pages[command.tab_id] {
                        removePage(page)
                    }
                    reply(command, outcome: .object(["kind": "error", "message": .string(error.localizedDescription)]))
                }
            }
        }
    }

    private func execute(_ command: NativeBrowserCommand) async throws -> JSONValue {
        guard command.generation == generation, Date().timeIntervalSince1970 * 1000 <= Double(command.deadline_ms) else { throw NativeBrowserFailure(message: "browser.command_expired: command belongs to an expired connection or deadline") }
        if case .recording(let kind, let value) = command.operation {
            guard let id = value["recording_id"]?.stringValue else { throw NativeBrowserFailure(message: "browser.recording_id_invalid") }
            switch kind {
            case "recording_start":
                guard let encoded = value["options"], let page = pages[command.tab_id] else { throw NativeBrowserFailure(message: "browser.tab_unavailable") }
                try page.validate(agent: command.agent, epoch: command.control_epoch, deadline: Double(command.deadline_ms), generation: command.generation)
                let options = try JSONDecoder().decode(NativeRecordingOptions.self, from: encoded.encoded())
                recording = (id, page.id)
                return try await recorder.start(id: id, options: options, page: page, agent: command.agent, epoch: command.control_epoch, deadline: Double(command.deadline_ms), generation: command.generation, owner: nil)
            case "recording_caption":
                if command.agent {
                    guard let page = pages[command.tab_id] else { throw NativeBrowserFailure(message: "browser.tab_unavailable") }
                    try page.validate(agent: true, epoch: command.control_epoch, deadline: Double(command.deadline_ms), generation: command.generation)
                }
                guard let captionID = value["caption_id"]?.stringValue, UUID(uuidString: captionID) != nil, let text = value["text"]?.stringValue else { throw NativeBrowserFailure(message: "browser.recording_caption_invalid") }
                return try await recorder.caption(id: id, tab: command.tab_id, captionID: captionID, text: text, owner: nil)
            case "recording_stop": return try await recorder.stop(id: id, tab: command.tab_id, owner: nil)
            case "recording_status": return try await recorder.status(id: id, tab: command.tab_id, owner: nil)
            case "recording_read":
                guard let artifact = value["artifact"]?.stringValue, let offset = value["offset"]?.doubleValue, offset >= 0, offset.rounded() == offset, offset <= 536870912, let maximum = value["max_bytes"]?.doubleValue, maximum > 0, maximum <= 262144, maximum.rounded() == maximum else { throw NativeBrowserFailure(message: "browser.recording_read_invalid") }
                return try await recorder.storage.read(id: id, tab: command.tab_id, kind: artifact, offset: UInt64(offset), maximum: Int(maximum), owner: nil)
            case "recording_release":
                guard let value = value["artifacts"] else { throw NativeBrowserFailure(message: "browser.recording_release_invalid") }
                let artifacts = try JSONDecoder().decode([NativeRecordingArtifact].self, from: value.encoded())
                try await recorder.storage.release(id: id, tab: command.tab_id, artifacts: artifacts, owner: nil)
                return .null
            default: throw NativeBrowserFailure(message: "browser.recording_operation_invalid")
            }
        }
        if case .ensure(let workspace, let profile, let url) = command.operation {
            guard profile == profileID else { throw NativeBrowserFailure(message: "browser.profile_mismatch: the requested profile does not match this host's profile") }
            if let existing = pages[command.tab_id] {
                guard existing.workspaceID == workspace, existing.profileID == profile else { throw NativeBrowserFailure(message: "browser.tab_scope_mismatch") }
                return existing.state
            }
            guard pages.count < Self.pageLimit else { throw NativeBrowserFailure(message: "browser.tab_limit: native page capacity is full") }
            let page = NativeBrowserPage(id: command.tab_id, workspaceID: workspace, profileID: profile, store: store)
            configure(page)
            do { try page.navigate(url) }
            catch {
                page.disconnect()
                page.renderWindow.close()
                throw error
            }
            pages[page.id] = page
            return page.state
        }
        guard let page = pages[command.tab_id] else { throw NativeBrowserFailure(message: "browser.tab_unavailable: restore this tab before using it") }
        if case .dialog(let action) = command.operation {
            guard command.agent else { throw NativeBrowserFailure(message: "browser.control_denied: dialogs are answered by agent commands") }
            try page.validate(agent: true, epoch: command.control_epoch, deadline: Double(command.deadline_ms), generation: command.generation, allowingDialog: true)
            return try page.dialogs.perform(action)
        }
        try page.validate(agent: command.agent, epoch: command.control_epoch, deadline: Double(command.deadline_ms), generation: command.generation)
        page.beginActivity()
        defer {
            page.endActivity()
            if command.agent { page.hold(milliseconds: 30000) }
        }
        switch command.operation {
        case .ensure, .recording, .dialog: break
        case .navigate(let url): try page.navigate(url)
        case .back: page.webView.goBack()
        case .forward: page.webView.goForward()
        case .reload: page.webView.reload()
        case .close: removePage(page)
        case .resize(let width, let height): try page.resize(width: width, height: height)
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
        send(.object(["type": "reply", "generation": .string(command.generation), "id": .string(command.id), "outcome": outcome]))
    }

    private func publish(_ page: NativeBrowserPage) {
        guard let generation, pages[page.id] === page else { return }
        send(.object(["type": "tab", "generation": .string(generation), "tab_id": .string(page.id), "state": page.state]))
    }

    private func send(_ value: JSONValue) {
        var data = value.encoded()
        data.append(10)
        guard StdioOutput.write(data) else {
            if !stopping { shutdown(code: 1, reason: "browser.transport_failed: stdout is closed") }
            return
        }
    }

    private func configure(_ page: NativeBrowserPage) {
        page.onAutomationTarget = { [weak self, weak page] rect, navigation in if let page { self?.recorder.target(page: page, rect: rect, navigation: navigation) } }
        page.onRecordingPointer = { [weak self, weak page] point, click in
            guard let self, let page else { return }
            recorder.pointer(page: page, point: point, click: click)
        }
        page.hostGeneration = generation
        page.onState = { [weak self, weak page] in if let page { self?.publish(page) } }
        page.onPopup = { _, _ in nil }
        page.onClose = {}
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
    }

    func shutdown(code: Int32, reason: String?) {
        guard !stopping else { return }
        stopping = true
        if let reason { StdioOutput.log(reason) }
        commands.removeAll()
        recorder.interrupt(reason: "Native host disconnected")
        let pending = recording
        let recorder = recorder
        Task {
            let deadline = ProcessInfo.processInfo.systemUptime + Self.shutdownGrace
            if let pending {
                while ProcessInfo.processInfo.systemUptime < deadline {
                    recorder.interrupt(reason: "Native host disconnected")
                    guard let state = try? await recorder.storage.existing(id: pending.id, tab: pending.tab), let phase = state["phase"]?.stringValue, ["starting", "recording", "stopping"].contains(phase) else { break }
                    try? await Task.sleep(for: .milliseconds(50))
                }
            }
            exit(code)
        }
    }
}
