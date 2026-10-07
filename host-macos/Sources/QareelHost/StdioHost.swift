import AppKit
import QareelEngine
import WebKit
import Darwin

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
    private static let shutdownGrace = 8.0

    private let lockFD: Int32
    private var engine: QareelEngine?
    private var scope: Int32 = 0
    private var stopping = false
    private var signalSources: [any DispatchSourceSignal] = []
    private var activity: (any NSObjectProtocol)?

    init(bootstrap: QareelBootstrap, lockFD: Int32) {
        self.lockFD = lockFD
        let engine = QareelEngine(profileID: bootstrap.profileID, store: WKWebsiteDataStore(forIdentifier: bootstrap.profileID), recordings: bootstrap.directory.appendingPathComponent("recordings", isDirectory: true), pageLimit: 8, presentation: "none", extraOperations: []) { [weak self] _, channel, data in
            self?.emitted(channel: channel, data: data)
        }
        self.engine = engine
        scope = engine.openScope(id: "stdio", remote: false, recordingOwner: nil, popups: true)
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
                receive(input)
                permits.signal()
            }
        }
    }

    private func receive(_ input: StdioInput) {
        guard !stopping else { return }
        switch input {
        case .end: shutdown(code: 0, reason: nil)
        case .oversized: shutdown(code: 1, reason: "browser.protocol_invalid: input line exceeds 8 MiB")
        case .failed(let code): shutdown(code: 1, reason: "browser.transport_failed: stdin read failed (errno \(code))")
        case .line(let data): engine?.submit(scope: scope, data: data)
        }
    }

    private func emitted(channel: Int32, data: Data) {
        if channel == EngineChannel.interface {
            if let value = JSONValue.parse(data), value["type"]?.stringValue == "protocol_failure" {
                shutdown(code: 1, reason: value["reason"]?.stringValue ?? "browser.protocol_invalid")
            }
            return
        }
        var line = data
        line.append(10)
        if !StdioOutput.write(line), !stopping { shutdown(code: 1, reason: "browser.transport_failed: stdout is closed") }
    }

    func shutdown(code: Int32, reason: String?) {
        guard !stopping else { return }
        stopping = true
        if let reason { StdioOutput.log(reason) }
        guard let engine else { exit(code) }
        engine.disconnect(scope: scope)
        engine.interruptAll(reason: "Native host disconnected")
        Task {
            let deadline = ProcessInfo.processInfo.systemUptime + Self.shutdownGrace
            while engine.recording, ProcessInfo.processInfo.systemUptime < deadline {
                try? await Task.sleep(for: .milliseconds(50))
            }
            exit(code)
        }
    }
}
