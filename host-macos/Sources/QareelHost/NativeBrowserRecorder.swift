import AppKit
import AVFoundation
import CoreMedia
import CoreVideo
import CoreGraphics
import CoreText
import CryptoKit
import Darwin
import WebKit

struct NativeRecordingOptions: Codable, Equatable, Sendable {
    struct Scope: Codable, Equatable, Sendable { let kind: String; let origins: [String]? }
    struct Overlays: Codable, Equatable, Sendable { let cursor: Bool; let clicks: Bool; let captions: Bool; let highlights: Bool }
    let scope: Scope
    let fps: Int
    let max_duration_ms: UInt64
    let max_bytes: UInt64
    let max_dimension: Int
    let control_policy: String
    let audio: String
    let overlays: Overlays

    func validate() throws {
        guard (1...30).contains(fps), (240...1280).contains(max_dimension), (1000...300000).contains(max_duration_ms), (1048576...104857600).contains(max_bytes), ["agent", "user"].contains(control_policy), ["off", "app", "microphone", "app_and_microphone"].contains(audio) else { throw NativeBrowserFailure(message: "browser.recording_options_invalid: recording limits or options are unsupported") }
        guard scope.kind == "local" || (scope.kind == "origins" && !(scope.origins ?? []).isEmpty && (scope.origins ?? []).count <= 16 && (scope.origins ?? []).allSatisfy({ Self.origin($0) == $0 })) else { throw NativeBrowserFailure(message: "browser.recording_scope_invalid: provide bounded canonical origins") }
    }

    static func origin(_ text: String) -> String? {
        guard let url = URLComponents(string: text), let scheme = url.scheme?.lowercased(), ["http", "https"].contains(scheme), let host = url.host?.lowercased(), url.user == nil, url.password == nil else { return nil }
        let port = url.port.flatMap { ($0 == 80 && scheme == "http") || ($0 == 443 && scheme == "https") ? nil : $0 }
        return "\(scheme)://\(host)" + (port.map { ":\($0)" } ?? "")
    }

    func allows(_ url: URL?) -> Bool {
        guard let url, let origin = Self.origin(url.absoluteString) else { return false }
        if scope.kind == "origins" { return scope.origins?.contains(origin) == true }
        let host = url.host?.lowercased() ?? ""
        return ["localhost", "127.0.0.1", "[::1]", "::1"].contains(host) || host.hasSuffix(".localhost")
    }
}

struct NativeRecordingArtifact: Codable, Equatable, Sendable {
    let kind: String
    let bytes: UInt64
    let sha256: String
}

private struct NativeRecordingCue: Codable, Sendable {
    let caption_id: String
    let text: String
    let time_ms: UInt64
}

private struct NativeRecordingMark: Codable, Sendable {
    let time_ms: UInt64
    let x: Double
    let y: Double
    let width: Double
    let height: Double
}

private struct NativeRecordingManifest: Codable, Sendable {
    let recording_id: String
    let tab_id: String
    var host_id: String?
    let workspace_id: String
    let options: NativeRecordingOptions
    let started_at_unix_ms: UInt64
    var phase: String
    var duration_ms: UInt64 = 0
    var frames: UInt64 = 0
    var dropped_frames: UInt64 = 0
    var audio_status: String
    var audio_gap_ms: UInt64 = 0
    var artifacts: [NativeRecordingArtifact] = []
    var reason: String?
    var captions: [NativeRecordingCue] = []
    var released = false
    var encoder_completed: Bool?
    var marks: [NativeRecordingMark]?
    var viewport_width: Double?
    var viewport_height: Double?

    var terminal: Bool { ["complete", "interrupted", "failed"].contains(phase) }
    var state: JSONValue {
        .object(["recording_id": .string(recording_id), "phase": .string(phase), "started_at_unix_ms": .number(Double(started_at_unix_ms)), "duration_ms": .number(Double(duration_ms)), "frames": .number(Double(frames)), "dropped_frames": .number(Double(dropped_frames)), "audio": .string(options.audio), "audio_status": .string(audio_status), "audio_gap_ms": .number(Double(audio_gap_ms)), "artifacts": .array(artifacts.map { .object(["kind": .string($0.kind), "bytes": .number(Double($0.bytes)), "sha256": .string($0.sha256)]) }), "reason": reason.map(JSONValue.string) ?? .null])
    }
}

struct NativeRecordingOverlay: Sendable {
    let point: CGPoint?
    let clickAge: Double?
    let viewport: CGSize
    let caption: String?
    let highlight: CGRect?
}

actor NativeRecordingStorage {
    private let directory: URL
    private var lockFD: Int32 = -1
    private var prepared = false
    private var expiredBefore: UInt64 = 0
    private var records: [String: NativeRecordingManifest] = [:]
    private var writer: AVAssetWriter?
    private var input: AVAssetWriterInput?
    private var adaptor: AVAssetWriterInputPixelBufferAdaptor?
    private var audioInput: AVAssetWriterInput?
    private var activeID: String?
    private var dimensions = CGSize.zero
    private var checkpoint: UInt64 = 0
    private var finishing: (UUID, CheckedContinuation<Bool, Never>)?
    private let files = ["video": "recording.mp4", "captions_srt": "captions.srt", "captions_vtt": "captions.vtt", "events": "events.json"]

    init(directory: URL) { self.directory = directory }
    deinit { if lockFD >= 0 { _ = flock(lockFD, LOCK_UN); _ = Darwin.close(lockFD) } }

    private func prepare() throws {
        guard !prepared else { return }
        if lockFD < 0 {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
            let fd = open(directory.appendingPathComponent("recorder.lock").path, O_CREAT | O_RDWR | O_NOFOLLOW | O_CLOEXEC, mode_t(0o600))
            guard fd >= 0 else { throw NativeBrowserFailure(message: "browser.recording_storage: cannot open recorder lock") }
            guard flock(fd, LOCK_EX | LOCK_NB) == 0 else { _ = Darwin.close(fd); throw NativeBrowserFailure(message: "browser.recording_busy: recorder storage is in use") }
            lockFD = fd
        }
        records.removeAll()
        let retired = directory.appendingPathComponent(".retired-before")
        if FileManager.default.fileExists(atPath: retired.path) {
            let bytes = try boundedRead(retired, limit: 32)
            guard let floor = UInt64(String(decoding: bytes, as: UTF8.self)) else { throw NativeBrowserFailure(message: "browser.recording_storage: retirement receipt is corrupt") }
            expiredBefore = floor
        }
        let entries = try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: [.isDirectoryKey], options: [.skipsHiddenFiles])
        guard entries.count <= 257 else { throw NativeBrowserFailure(message: "browser.recording_storage_limit: recording inventory is full") }
        for entry in entries where UUID(uuidString: entry.lastPathComponent) != nil {
            let values = try entry.resourceValues(forKeys: [.isDirectoryKey, .isSymbolicLinkKey])
            guard values.isDirectory == true, values.isSymbolicLink != true else { throw NativeBrowserFailure(message: "browser.recording_storage: invalid recording directory") }
            let file = entry.appendingPathComponent("manifest.json")
            let data = try boundedRead(file, limit: 1048576)
            var record = try JSONDecoder().decode(NativeRecordingManifest.self, from: data)
            guard record.recording_id == entry.lastPathComponent else { throw NativeBrowserFailure(message: "browser.recording_storage: recording identity mismatch") }
            if !record.terminal {
                if record.encoder_completed != true {
                    record.reason = "The native host stopped before recording finalized; partial fragments were preserved"
                }
                record = try finalizeArtifacts(record)
            }
            records[record.recording_id] = record
            if record.released { try deleteArtifacts(record.recording_id) }
        }
        prepared = true
        try retireExpired()
    }

    private func timestamp(_ id: String) -> UInt64? {
        guard id.count == 36, id[id.index(id.startIndex, offsetBy: 14)] == "7" else { return nil }
        return UInt64(id.replacingOccurrences(of: "-", with: "").prefix(12), radix: 16)
    }

    private func retireExpired() throws {
        let now = UInt64(Date().timeIntervalSince1970 * 1000)
        let expired = records.values.filter { record in
            record.released && timestamp(record.recording_id).map { $0 + 86700000 < now } == true
        }
        let floor = expired.compactMap { timestamp($0.recording_id) }.max() ?? expiredBefore
        if floor > expiredBefore {
            try durableWrite(Data(String(floor).utf8), to: directory.appendingPathComponent(".retired-before"))
            expiredBefore = floor
        }
        for record in expired {
            try deleteArtifacts(record.recording_id)
            try FileManager.default.removeItem(at: folder(record.recording_id))
            records.removeValue(forKey: record.recording_id)
        }
    }

    private func folder(_ id: String) -> URL { directory.appendingPathComponent(id, isDirectory: true) }

    private func boundedRead(_ url: URL, limit: Int) throws -> Data {
        let values = try url.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey, .fileSizeKey])
        guard values.isRegularFile == true, values.isSymbolicLink != true, let size = values.fileSize, size <= limit else { throw NativeBrowserFailure(message: "browser.recording_storage: invalid or oversized file") }
        return try Data(contentsOf: url)
    }

    private func durableWrite(_ data: Data, to url: URL) throws {
        let temporary = url.deletingLastPathComponent().appendingPathComponent(".\(UUID().uuidString).pending")
        let fd = open(temporary.path, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, mode_t(0o600))
        guard fd >= 0 else { throw NativeBrowserFailure(message: "browser.recording_storage: cannot create recording metadata") }
        defer { _ = Darwin.close(fd); _ = unlink(temporary.path) }
        try data.withUnsafeBytes { buffer in
            var offset = 0
            while offset < buffer.count {
                let count = Darwin.write(fd, buffer.baseAddress!.advanced(by: offset), buffer.count - offset)
                if count < 0 && errno == EINTR { continue }
                guard count > 0 else { throw NativeBrowserFailure(message: "browser.recording_storage: metadata write failed") }
                offset += count
            }
        }
        guard fsync(fd) == 0, rename(temporary.path, url.path) == 0 else { throw NativeBrowserFailure(message: "browser.recording_storage: metadata commit failed") }
        let parent = open(url.deletingLastPathComponent().path, O_RDONLY | O_DIRECTORY | O_CLOEXEC)
        guard parent >= 0 else { throw NativeBrowserFailure(message: "browser.recording_storage: metadata directory unavailable") }
        defer { _ = Darwin.close(parent) }
        guard fsync(parent) == 0 else { throw NativeBrowserFailure(message: "browser.recording_storage: metadata directory sync failed") }
    }

    private func save(_ record: NativeRecordingManifest) throws { try durableWrite(JSONEncoder().encode(record), to: folder(record.recording_id).appendingPathComponent("manifest.json")) }

    func existing(id: String, tab: String, options: NativeRecordingOptions? = nil, owner: String? = nil) throws -> JSONValue? {
        try prepare()
        guard let record = records[id] else { return nil }
        guard record.host_id == owner, record.tab_id == tab, options == nil || options == record.options else { throw NativeBrowserFailure(message: "browser.recording_identity_conflict: recording ID belongs to different immutable options or tab") }
        return record.state
    }

    func start(id: String, pageID: String, workspace: String, options: NativeRecordingOptions, size: CGSize, syntheticAudio: Bool = false, owner: String? = nil) throws -> JSONValue {
        try prepare()
        if let existing = try existing(id: id, tab: pageID, options: options, owner: owner) { return existing }
        try retireExpired()
        let now = UInt64(Date().timeIntervalSince1970 * 1000)
        guard let created = timestamp(id), created > expiredBefore, created <= now + 300000, now <= created + 86400000 else { throw NativeBrowserFailure(message: "browser_recording.expired_id: unknown recording ID is outside the start horizon") }
        guard activeID == nil, records.count < 256 else { throw NativeBrowserFailure(message: "browser.recording_busy: one recorder or the bounded recording inventory is full") }
        var used: UInt64 = 0
        for record in records.values {
            for name in files.values {
                let file = folder(record.recording_id).appendingPathComponent(name)
                if FileManager.default.fileExists(atPath: file.path) {
                    let values = try file.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey, .fileSizeKey])
                    guard values.isRegularFile == true, values.isSymbolicLink != true, let size = values.fileSize else { throw NativeBrowserFailure(message: "browser.recording_storage: invalid retained artifact") }
                    used += UInt64(size)
                }
            }
        }
        guard used + options.max_bytes + 4 * 1024 * 1024 <= 512 * 1024 * 1024 else { throw NativeBrowserFailure(message: "browser.recording_storage_limit: release completed recordings before starting another") }
        try FileManager.default.createDirectory(at: folder(id), withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
        var record = NativeRecordingManifest(recording_id: id, tab_id: pageID, host_id: owner, workspace_id: workspace, options: options, started_at_unix_ms: UInt64(Date().timeIntervalSince1970 * 1000), phase: "starting", audio_status: options.audio == "off" ? "disabled" : "pending")
        try save(record)
        records[id] = record
        guard options.audio == "off" || syntheticAudio else {
            record.phase = "failed"; record.audio_status = "failed"; record.reason = "Requested audio capture is unavailable until its source isolation and permission flow are verified"
            try save(record); records[id] = record
            return record.state
        }
        do {
            let output = folder(id).appendingPathComponent("recording.mp4")
            let writer = try AVAssetWriter(outputURL: output, fileType: .mp4)
            writer.movieFragmentInterval = CMTime(seconds: 1, preferredTimescale: 600)
            writer.shouldOptimizeForNetworkUse = false
            let input = AVAssetWriterInput(mediaType: .video, outputSettings: [AVVideoCodecKey: AVVideoCodecType.h264, AVVideoWidthKey: Int(size.width), AVVideoHeightKey: Int(size.height), AVVideoCompressionPropertiesKey: [AVVideoAverageBitRateKey: 4_000_000, AVVideoMaxKeyFrameIntervalKey: options.fps, AVVideoAllowFrameReorderingKey: false]])
            input.expectsMediaDataInRealTime = true
            guard writer.canAdd(input) else { throw NativeBrowserFailure(message: "browser.recording_encoder: video encoder unavailable") }
            writer.add(input)
            let attributes: [String: Any] = [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA, kCVPixelBufferWidthKey as String: Int(size.width), kCVPixelBufferHeightKey as String: Int(size.height), kCVPixelBufferCGImageCompatibilityKey as String: true, kCVPixelBufferCGBitmapContextCompatibilityKey as String: true]
            let adaptor = AVAssetWriterInputPixelBufferAdaptor(assetWriterInput: input, sourcePixelBufferAttributes: attributes)
            if syntheticAudio {
                let audio = AVAssetWriterInput(mediaType: .audio, outputSettings: [AVFormatIDKey: kAudioFormatMPEG4AAC, AVSampleRateKey: 48000, AVNumberOfChannelsKey: 2, AVEncoderBitRateKey: 128000])
                audio.expectsMediaDataInRealTime = true
                guard writer.canAdd(audio) else { throw NativeBrowserFailure(message: "browser.recording_encoder: audio encoder unavailable") }
                writer.add(audio); audioInput = audio
            }
            guard writer.startWriting() else { throw writer.error ?? NativeBrowserFailure(message: "browser.recording_encoder: encoder did not start") }
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: output.path)
            writer.startSession(atSourceTime: .zero)
            self.writer = writer; self.input = input; self.adaptor = adaptor; dimensions = size; activeID = id; checkpoint = 0
            record.phase = "recording"; try save(record); records[id] = record
            return record.state
        } catch {
            record.phase = "failed"; record.reason = "Native encoder could not start"
            writer?.cancelWriting(); writer = nil; input = nil; adaptor = nil; audioInput = nil; activeID = nil
            try save(record); records[id] = record
            throw error
        }
    }

    func append(id: String, image: CGImage, milliseconds: UInt64, overlay: NativeRecordingOverlay) throws -> Bool {
        guard activeID == id, let writer, let input, let adaptor, var record = records[id], record.phase == "recording" else { return false }
        guard writer.status == .writing else { throw NativeBrowserFailure(message: "browser.recording_encoder: encoder stopped accepting frames") }
        record.duration_ms = milliseconds
        guard input.isReadyForMoreMediaData else { record.dropped_frames += 1; records[id] = record; return true }
        var pixel: CVPixelBuffer?
        let threshold = [kCVPixelBufferPoolAllocationThresholdKey as String: 2] as CFDictionary
        guard let pool = adaptor.pixelBufferPool, CVPixelBufferPoolCreatePixelBufferWithAuxAttributes(nil, pool, threshold, &pixel) == kCVReturnSuccess, let pixel else { record.dropped_frames += 1; records[id] = record; return true }
        CVPixelBufferLockBaseAddress(pixel, [])
        defer { CVPixelBufferUnlockBaseAddress(pixel, []) }
        guard let base = CVPixelBufferGetBaseAddress(pixel), let context = CGContext(data: base, width: Int(dimensions.width), height: Int(dimensions.height), bitsPerComponent: 8, bytesPerRow: CVPixelBufferGetBytesPerRow(pixel), space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue) else { throw NativeBrowserFailure(message: "browser.recording_encoder: pixel allocation failed") }
        context.setFillColor(CGColor(gray: 0, alpha: 1)); context.fill(CGRect(origin: .zero, size: dimensions))
        let fit = min(dimensions.width / CGFloat(image.width), dimensions.height / CGFloat(image.height))
        let frame = CGRect(x: (dimensions.width - CGFloat(image.width) * fit) / 2, y: (dimensions.height - CGFloat(image.height) * fit) / 2, width: CGFloat(image.width) * fit, height: CGFloat(image.height) * fit)
        context.draw(image, in: frame)
        if let point = overlay.point, overlay.viewport.width > 0, overlay.viewport.height > 0 {
            let x = frame.minX + point.x / overlay.viewport.width * frame.width
            let y = frame.maxY - point.y / overlay.viewport.height * frame.height
            if record.options.overlays.cursor {
                context.setFillColor(CGColor(gray: 1, alpha: 0.95)); context.setStrokeColor(CGColor(gray: 0, alpha: 0.9)); context.setLineWidth(2)
                context.addEllipse(in: CGRect(x: x - 5, y: y - 5, width: 10, height: 10)); context.drawPath(using: .fillStroke)
            }
            if record.options.overlays.clicks, let age = overlay.clickAge, age < 0.6 {
                let radius = 12 + age * 30
                context.setStrokeColor(CGColor(red: 0.1, green: 0.65, blue: 1, alpha: 1 - age / 0.6)); context.setLineWidth(3)
                context.strokeEllipse(in: CGRect(x: x - radius, y: y - radius, width: radius * 2, height: radius * 2))
            }
        }
        if record.options.overlays.highlights, let rect = overlay.highlight, overlay.viewport.width > 0, overlay.viewport.height > 0 {
            let transformed = CGRect(x: frame.minX + rect.minX / overlay.viewport.width * frame.width, y: frame.maxY - rect.maxY / overlay.viewport.height * frame.height, width: rect.width / overlay.viewport.width * frame.width, height: rect.height / overlay.viewport.height * frame.height)
            context.setStrokeColor(CGColor(red: 1, green: 0.7, blue: 0.1, alpha: 0.95)); context.setLineWidth(3); context.stroke(transformed)
        }
        if record.options.overlays.captions, let caption = overlay.caption {
            let font = CTFontCreateWithName("Helvetica-Bold" as CFString, 22, nil)
            let attributes: [NSAttributedString.Key: Any] = [NSAttributedString.Key(kCTFontAttributeName as String): font, NSAttributedString.Key(kCTForegroundColorAttributeName as String): CGColor(gray: 1, alpha: 1)]
            let line = CTLineCreateWithAttributedString(NSAttributedString(string: caption.replacingOccurrences(of: "\n", with: " "), attributes: attributes))
            let available = dimensions.width - 48
            let rendered = CTLineCreateTruncatedLine(line, available, .end, CTLineCreateWithAttributedString(NSAttributedString(string: "…", attributes: attributes))) ?? line
            let width = min(available, CTLineGetTypographicBounds(rendered, nil, nil, nil))
            context.setFillColor(CGColor(gray: 0, alpha: 0.75)); context.fill(CGRect(x: (dimensions.width - width) / 2 - 12, y: 18, width: width + 24, height: 40))
            context.textPosition = CGPoint(x: (dimensions.width - width) / 2, y: 30); CTLineDraw(rendered, context)
        }
        guard adaptor.append(pixel, withPresentationTime: CMTime(value: Int64(milliseconds), timescale: 1000)) else { throw NativeBrowserFailure(message: "browser.recording_encoder: frame append failed") }
        record.frames += 1; records[id] = record
        if milliseconds >= checkpoint + 1000 {
            checkpoint = milliseconds
            try save(record)
            let size = try folder(id).appendingPathComponent("recording.mp4").resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0
            return UInt64(size) < record.options.max_bytes
        }
        return true
    }

    func caption(id: String, tab: String, captionID: String, text: String, milliseconds: UInt64, owner: String? = nil) throws -> JSONValue {
        try prepare()
        guard var record = records[id], record.host_id == owner, record.tab_id == tab else { throw NativeBrowserFailure(message: "browser_recording.not_found") }
        if let existing = record.captions.first(where: { $0.caption_id == captionID }) {
            guard existing.text == text else { throw NativeBrowserFailure(message: "browser.recording_caption_conflict") }
            return .object(["caption_id": .string(captionID), "time_ms": .number(Double(existing.time_ms))])
        }
        do { _ = try BrowserRecordingCue(id: captionID, text: text, startMilliseconds: milliseconds, endMilliseconds: milliseconds + 1) }
        catch { throw NativeBrowserFailure(message: "browser.recording_caption_invalid: caption does not meet format limits") }
        guard record.captions.reduce(0, { $0 + $1.text.utf8.count }) + text.utf8.count <= 262144, record.phase == "recording", record.captions.count < 1000, !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, text.utf8.count <= 4096 else { throw NativeBrowserFailure(message: "browser.recording_caption_invalid: recording must be active and caption bounded") }
        let proposed = record.captions + [NativeRecordingCue(caption_id: captionID, text: text, time_ms: milliseconds)]
        do { _ = try BrowserRecordingCaptions(cues: proposed.map { try BrowserRecordingCue(id: $0.caption_id, text: $0.text, startMilliseconds: $0.time_ms, endMilliseconds: $0.time_ms + 1) }) }
        catch { throw NativeBrowserFailure(message: "browser.recording_caption_invalid: caption track does not meet format limits") }
        record.captions = proposed
        try save(record); records[id] = record
        return .object(["caption_id": .string(captionID), "time_ms": .number(Double(milliseconds))])
    }

    func mark(id: String, rect: CGRect, viewport: CGSize, milliseconds: UInt64) throws {
        guard var record = records[id], record.phase == "recording", (record.marks?.count ?? 0) < 2000 else { return }
        record.marks = (record.marks ?? []) + [NativeRecordingMark(time_ms: milliseconds, x: rect.minX, y: rect.minY, width: rect.width, height: rect.height)]
        record.viewport_width = viewport.width
        record.viewport_height = viewport.height
        try save(record); records[id] = record
    }

    func requestStop(id: String, tab: String, owner: String? = nil) throws -> JSONValue {
        try prepare()
        guard var record = records[id], record.host_id == owner, record.tab_id == tab else { throw NativeBrowserFailure(message: "browser_recording.not_found") }
        if !record.terminal { record.phase = "stopping"; try save(record); records[id] = record }
        return record.state
    }

    func stop(id: String, reason: String?, milliseconds: UInt64) async throws -> JSONValue {
        try prepare()
        guard var record = records[id] else { throw NativeBrowserFailure(message: "browser_recording.not_found") }
        if record.terminal { return record.state }
        guard activeID == id else { throw NativeBrowserFailure(message: "browser_recording.not_found") }
        if finishing != nil { return record.state }
        record.phase = "stopping"; record.duration_ms = max(record.duration_ms, milliseconds); record.reason = reason ?? record.reason
        records[id] = record
        try save(record)
        var timedOut = false
        if let writer, writer.status == .writing {
            input?.markAsFinished(); audioInput?.markAsFinished()
            let token = UUID()
            let finished = await withCheckedContinuation { continuation in
                finishing = (token, continuation)
                writer.finishWriting { Task { await self.finished(token, success: true) } }
                Task {
                    try? await Task.sleep(for: .seconds(5))
                    self.finished(token, success: false)
                }
            }
            if !finished { timedOut = true; writer.cancelWriting() }
        }
        record = records[id] ?? record
        if record.frames == 0 {
            record.reason = record.reason ?? "Recording stopped before any usable frame was captured"
        } else if writer?.status == .completed {
            try synchronizeVideo(id)
            record.encoder_completed = true
            records[id] = record
            try save(record)
        } else {
            record.reason = [record.reason, encoderFailure(timedOut: timedOut)].compactMap { $0 }.joined(separator: "; ")
        }
        let finalized = try finalizeArtifacts(record)
        records[id] = finalized
        writer = nil; input = nil; adaptor = nil; audioInput = nil; activeID = nil
        return finalized.state
    }

    private func encoderFailure(timedOut: Bool) -> String {
        var details = ["status=\(writer?.status.rawValue ?? -1)"]
        var error = writer?.error as NSError?
        for _ in 0..<3 {
            guard let current = error else { break }
            let domain = String(current.domain.unicodeScalars.filter { CharacterSet.alphanumerics.contains($0) || "._-".unicodeScalars.contains($0) }.prefix(80))
            details.append("\(domain):\(current.code)")
            error = current.userInfo[NSUnderlyingErrorKey] as? NSError
        }
        let summary = timedOut ? "Native encoder finalization timed out" : "Native encoder did not finalize"
        return "\(summary) (\(details.joined(separator: ", "))); available fragments were preserved"
    }

    private func synchronizeVideo(_ id: String) throws {
        let fd = open(folder(id).appendingPathComponent("recording.mp4").path, O_RDONLY | O_NOFOLLOW | O_CLOEXEC)
        guard fd >= 0 else { throw NativeBrowserFailure(message: "browser.recording_storage: finalized video is missing") }
        defer { _ = Darwin.close(fd) }
        var info = stat()
        guard fstat(fd, &info) == 0, info.st_mode & S_IFMT == S_IFREG, info.st_size > 0, fsync(fd) == 0 else { throw NativeBrowserFailure(message: "browser.recording_storage: finalized video could not be synchronized") }
    }

    private func finalizeArtifacts(_ input: NativeRecordingManifest) throws -> NativeRecordingManifest {
        var record = input
        let id = record.recording_id
        if record.frames == 0 || record.duration_ms == 0 { record.reason = record.reason ?? "Recording stopped before any usable frame was captured" }
        if record.encoder_completed == true { try synchronizeVideo(id) }
        let events = JSONValue.object(["recording_id": .string(id), "frames": .number(Double(record.frames)), "dropped_frames": .number(Double(record.dropped_frames)), "duration_ms": .number(Double(record.duration_ms)), "captions": .array(record.captions.map { .object(["caption_id": .string($0.caption_id), "time_ms": .number(Double($0.time_ms)), "text": .string($0.text)]) }), "viewport_width": .number(record.viewport_width ?? 0), "viewport_height": .number(record.viewport_height ?? 0), "marks": .array((record.marks ?? []).map { .object(["time_ms": .number(Double($0.time_ms)), "x": .number($0.x), "y": .number($0.y), "width": .number($0.width), "height": .number($0.height)]) })])
        try durableWrite(events.encoded(), to: folder(id).appendingPathComponent("events.json"))
        let subtitles = try subtitles(record)
        try durableWrite(Data(subtitles.0.utf8), to: folder(id).appendingPathComponent("captions.srt"))
        try durableWrite(Data(subtitles.1.utf8), to: folder(id).appendingPathComponent("captions.vtt"))
        record.artifacts = try artifacts(record)
        record.phase = record.encoder_completed == true && record.reason == nil ? "complete" : "interrupted"
        try save(record)
        return record
    }

    private func finished(_ token: UUID, success: Bool) {
        guard let current = finishing, current.0 == token else { return }
        finishing = nil
        current.1.resume(returning: success)
    }

    private func subtitles(_ record: NativeRecordingManifest) throws -> (String, String) {
        var cues: [BrowserRecordingCue] = []
        for (index, cue) in record.captions.enumerated() {
            let next = index + 1 < record.captions.count ? record.captions[index + 1].time_ms : record.duration_ms
            let end = min(record.duration_ms, min(cue.time_ms + 4000, next))
            guard end > cue.time_ms else { continue }
            cues.append(try BrowserRecordingCue(id: cue.caption_id, text: cue.text, startMilliseconds: cue.time_ms, endMilliseconds: end))
        }
        let captions = try BrowserRecordingCaptions(cues: cues, durationMilliseconds: record.duration_ms)
        return (captions.srt(), captions.webVTT())
    }

    private func artifacts(_ record: NativeRecordingManifest) throws -> [NativeRecordingArtifact] {
        if record.released { return record.artifacts }
        return try files.sorted(by: { $0.key < $1.key }).compactMap { kind, name in
            let url = folder(record.recording_id).appendingPathComponent(name)
            guard FileManager.default.fileExists(atPath: url.path) else { return nil }
            let values = try url.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey, .fileSizeKey])
            guard values.isRegularFile == true, values.isSymbolicLink != true, let size = values.fileSize, size <= Int(record.options.max_bytes) + 4 * 1024 * 1024 else { throw NativeBrowserFailure(message: "browser.recording_storage: invalid artifact") }
            let handle = try FileHandle(forReadingFrom: url)
            defer { try? handle.close() }
            try handle.synchronize()
            var hash = SHA256()
            while let bytes = try handle.read(upToCount: 65536), !bytes.isEmpty { hash.update(data: bytes) }
            return NativeRecordingArtifact(kind: kind, bytes: UInt64(size), sha256: hash.finalize().map { String(format: "%02x", $0) }.joined())
        }
    }

    func read(id: String, tab: String, kind: String, offset: UInt64, maximum: Int, owner: String? = nil) throws -> JSONValue {
        try prepare()
        guard let record = records[id] else { throw NativeBrowserFailure(message: "browser_recording.not_found") }
        guard record.host_id == owner, record.tab_id == tab, record.terminal, !record.released, let artifact = record.artifacts.first(where: { $0.kind == kind }), let name = files[kind], maximum > 0, maximum <= 262144, offset <= artifact.bytes else { throw NativeBrowserFailure(message: "browser.recording_read_invalid") }
        let handle = try FileHandle(forReadingFrom: folder(id).appendingPathComponent(name))
        defer { try? handle.close() }
        try handle.seek(toOffset: offset)
        let data = try handle.read(upToCount: min(maximum, Int(artifact.bytes - offset))) ?? Data()
        return .object(["offset": .number(Double(offset)), "data_base64": .string(data.base64EncodedString()), "eof": .bool(offset + UInt64(data.count) == artifact.bytes), "total_bytes": .number(Double(artifact.bytes))])
    }

    func release(id: String, tab: String, artifacts: [NativeRecordingArtifact], owner: String? = nil) throws {
        try prepare()
        guard var record = records[id] else { throw NativeBrowserFailure(message: "browser_recording.not_found") }
        guard record.host_id == owner, record.tab_id == tab, record.terminal, artifacts.sorted(by: { $0.kind < $1.kind }) == record.artifacts.sorted(by: { $0.kind < $1.kind }) else { throw NativeBrowserFailure(message: "browser.recording_release_invalid: exact completed artifact descriptors required") }
        if !record.released { record.released = true; try save(record); records[id] = record }
        try deleteArtifacts(id)
    }

    private func deleteArtifacts(_ id: String) throws {
        for name in files.values { let url = folder(id).appendingPathComponent(name); if FileManager.default.fileExists(atPath: url.path) { try FileManager.default.removeItem(at: url) } }
    }
}

@MainActor
final class NativeBrowserRecorder {
    let storage: NativeRecordingStorage
    private var task: Task<Void, Never>?
    private weak var page: NativeBrowserPage?
    private var activeID: String?
    private var options: NativeRecordingOptions?
    private var started = 0.0
    private var stoppedReason: String?
    private var stopping = false
    private var point: CGPoint?
    private var clicked: Double?
    private var captionText: String?
    private var captionTime = 0.0
    private var snapshotPending = false
    private var highlight: (CGRect, UInt64, UInt64, CGSize, Double)?

    init(directory: URL? = nil) { storage = NativeRecordingStorage(directory: directory ?? FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".commission/native-browser-recordings", isDirectory: true)) }
    private var milliseconds: UInt64 { UInt64(max(0, (ProcessInfo.processInfo.systemUptime - started) * 1000)) }

    func start(id: String, options: NativeRecordingOptions, page: NativeBrowserPage, agent: Bool, epoch: UInt64, deadline: Double, generation: String, owner: String? = nil) async throws -> JSONValue {
        try options.validate()
        if let existing = try await storage.existing(id: id, tab: page.id, options: options, owner: owner) { return existing }
        try page.validate(agent: agent, epoch: epoch, deadline: deadline, generation: generation)
        guard options.control_policy == (agent ? "agent" : "user") else { throw NativeBrowserFailure(message: "browser.recording_control_invalid") }
        guard task == nil, !snapshotPending else { throw NativeBrowserFailure(message: "browser.recording_busy: one recording or snapshot is already active") }
        guard options.allows(page.webView.url), !page.dialogs.pending else { throw NativeBrowserFailure(message: "browser.recording_scope_invalid: current page is outside the granted recording scope") }
        let bounds = page.webView.bounds.size
        let scale = min(1, CGFloat(options.max_dimension) / max(bounds.width, bounds.height))
        let size = CGSize(width: max(2, floor(bounds.width * scale / 2) * 2), height: max(2, floor(bounds.height * scale / 2) * 2))
        let state = try await storage.start(id: id, pageID: page.id, workspace: page.workspaceID, options: options, size: size, owner: owner)
        guard state["phase"]?.stringValue == "recording" else { return state }
        do { try page.validate(agent: agent, epoch: epoch, deadline: deadline, generation: generation) }
        catch { return try await storage.stop(id: id, reason: "Recording start expired before capture", milliseconds: 0) }
        self.page = page; self.options = options; activeID = id; started = ProcessInfo.processInfo.systemUptime; stoppedReason = nil; stopping = false; point = nil; clicked = nil; captionText = nil
        highlight = nil
        page.beginActivity()
        task = Task { [weak self, weak page] in
            guard let self, let page else { return }
            defer { page.endActivity(); self.task = nil; self.activeID = nil; self.page = nil; self.options = nil }
            do {
                var index = 0
                while !stopping, !Task.isCancelled {
                    let elapsed = milliseconds
                    if elapsed >= options.max_duration_ms { stoppedReason = "Recording duration limit reached"; break }
                    if page.hostGeneration != generation || !options.allows(page.webView.url) || page.dialogs.pending || (options.control_policy == "agent" && page.human) { stoppedReason = "Recording stopped because page scope, dialog or control changed"; break }
                    let navigation = page.navigationEpoch
                    guard try await recordingSafe(page) else { stoppedReason = "Password field detected or recording safety state unavailable"; break }
                    guard !stopping, navigation == page.navigationEpoch, !page.dialogs.pending, options.control_policy != "agent" || !page.human else { continue }
                    let image = try await snapshot(page, width: size.width)
                    guard !stopping, navigation == page.navigationEpoch, options.allows(page.webView.url), !page.dialogs.pending, options.control_policy != "agent" || !page.human else { continue }
                    guard try await recordingSafe(page) else { stoppedReason = "Password field detected during capture; frame discarded"; break }
                    guard !stopping, navigation == page.navigationEpoch, page.hostGeneration == generation, options.allows(page.webView.url), !page.dialogs.pending, options.control_policy != "agent" || !page.human else { continue }
                    let now = ProcessInfo.processInfo.systemUptime
                    let target = highlight.flatMap { rect, navigation, control, size, time in
                        navigation == page.navigationEpoch && control == page.controlEpoch && size == page.webView.bounds.size && now - time <= 1 ? rect : nil
                    }
                    let overlay = NativeRecordingOverlay(point: point, clickAge: clicked.map { now - $0 }, viewport: page.webView.bounds.size, caption: now - captionTime < 4 ? captionText : nil, highlight: target)
                    guard try await storage.append(id: id, image: image, milliseconds: milliseconds, overlay: overlay) else { if !stopping { stoppedReason = "Recording byte limit reached" }; break }
                    index += 1
                    let next = started + Double(index) / Double(options.fps)
                    let delay = next - ProcessInfo.processInfo.systemUptime
                    if delay > 0 { try await Task.sleep(for: .seconds(delay)) }
                    else { index = Int((ProcessInfo.processInfo.systemUptime - started) * Double(options.fps)) }
                }
            } catch { stoppedReason = stoppedReason ?? "Recording capture failed; available frames were preserved" }
            do { _ = try await storage.stop(id: id, reason: stoppedReason, milliseconds: milliseconds) }
            catch { stoppedReason = "Recording finalization failed; preserved files require recovery" }
        }
        return state
    }

    private func recordingSafe(_ page: NativeBrowserPage) async throws -> Bool {
        let result = try await page.evaluate("Boolean(document.querySelector('input[type=password],input[autocomplete=current-password],input[autocomplete*=password i]'))", isolated: true)
        return result.boolValue == false
    }

    func target(page: NativeBrowserPage, rect: CGRect, navigation: UInt64) {
        guard self.page === page else { return }
        highlight = (rect, navigation, page.controlEpoch, page.webView.bounds.size, ProcessInfo.processInfo.systemUptime)
        if let id = activeID {
            let elapsed = milliseconds, viewport = page.webView.bounds.size
            Task { try? await storage.mark(id: id, rect: rect, viewport: viewport, milliseconds: elapsed) }
        }
    }

    private func snapshot(_ page: NativeBrowserPage, width: CGFloat) async throws -> CGImage {
        guard !snapshotPending else { throw NativeBrowserFailure(message: "browser.recording_snapshot_pending") }
        snapshotPending = true
        return try await withCheckedThrowingContinuation { continuation in
            var completion: CheckedContinuation<CGImage, any Error>? = continuation
            let timeout = Task { @MainActor in
                try? await Task.sleep(for: .seconds(8))
                guard !Task.isCancelled, let pending = completion else { return }
                completion = nil
                pending.resume(throwing: NativeBrowserFailure(message: "browser.recording_snapshot_timeout"))
            }
            let configuration = WKSnapshotConfiguration(); configuration.snapshotWidth = NSNumber(value: Double(width / max(1, page.webView.window?.backingScaleFactor ?? 1)))
            page.webView.takeSnapshot(with: configuration) { [weak self] image, error in
                self?.snapshotPending = false; timeout.cancel()
                guard let pending = completion else { return }
                completion = nil
                guard let image, let cgImage = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else { pending.resume(throwing: error ?? NativeBrowserFailure(message: "browser.recording_snapshot_failed")); return }
                pending.resume(returning: cgImage)
            }
        }
    }

    func pointer(page: NativeBrowserPage, point: CGPoint, click: Bool) {
        guard self.page === page else { return }
        self.point = point
        if click { clicked = ProcessInfo.processInfo.systemUptime }
    }

    func interrupt(page: NativeBrowserPage? = nil, reason: String) {
        guard activeID != nil, page == nil || self.page === page else { return }
        stoppedReason = reason; stopping = true
    }

    func stop(id: String, tab: String, owner: String? = nil) async throws -> JSONValue {
        guard try await storage.existing(id: id, tab: tab, owner: owner) != nil else { throw NativeBrowserFailure(message: "browser_recording.not_found") }
        if activeID != id { return try await status(id: id, tab: tab, owner: owner) }
        stopping = true
        return try await storage.requestStop(id: id, tab: tab, owner: owner)
    }

    func status(id: String, tab: String, owner: String? = nil) async throws -> JSONValue {
        guard let state = try await storage.existing(id: id, tab: tab, owner: owner) else { throw NativeBrowserFailure(message: "browser_recording.not_found") }
        if activeID != id, ["starting", "recording", "stopping"].contains(state["phase"]?.stringValue ?? "") {
            return try await storage.stop(id: id, reason: nil, milliseconds: UInt64(state["duration_ms"]?.doubleValue ?? 0))
        }
        return state
    }

    func caption(id: String, tab: String, captionID: String, text: String, owner: String? = nil) async throws -> JSONValue {
        let result = try await storage.caption(id: id, tab: tab, captionID: captionID, text: text, milliseconds: activeID == id ? milliseconds : 0, owner: owner)
        if activeID == id { captionText = text; captionTime = started + Double(result["time_ms"]?.doubleValue ?? 0) / 1000 }
        return result
    }
}
