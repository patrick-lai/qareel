import Foundation

public enum BrowserRecordingCaptionError: Error, LocalizedError, Sendable, Equatable {
    case invalidCue
    case invalidFormat
    case limitExceeded

    public var errorDescription: String? {
        switch self {
        case .invalidCue: "browser.recording_caption_invalid: captions require ordered, bounded times and plain text"
        case .invalidFormat: "browser.recording_caption_format: invalid SRT caption file"
        case .limitExceeded: "browser.recording_caption_limit: caption limits exceeded"
        }
    }
}

public struct BrowserRecordingCue: Sendable, Equatable {
    public let id: String
    public let text: String
    public let startMilliseconds: UInt64
    public let endMilliseconds: UInt64

    public init(id: String, text: String, startMilliseconds: UInt64, endMilliseconds: UInt64) throws {
        guard text.utf8.count <= 4096, id.utf8.count <= 128 else { throw BrowserRecordingCaptionError.limitExceeded }
        let normalized = text.replacingOccurrences(of: "\r\n", with: "\n").replacingOccurrences(of: "\r", with: "\n")
        guard !id.isEmpty, id.utf8.allSatisfy({ (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) || $0 == 45 || $0 == 95 }),
              startMilliseconds < endMilliseconds, endMilliseconds <= BrowserRecordingCaptions.maximumDurationMilliseconds,
              !normalized.isEmpty,
              normalized.unicodeScalars.allSatisfy({ !CharacterSet.controlCharacters.contains($0) || $0 == "\n" || $0 == "\t" }),
              normalized.split(separator: "\n", omittingEmptySubsequences: false).allSatisfy({ !$0.trimmingCharacters(in: .whitespaces).isEmpty }) else {
            throw BrowserRecordingCaptionError.invalidCue
        }
        self.id = id
        self.text = normalized
        self.startMilliseconds = startMilliseconds
        self.endMilliseconds = endMilliseconds
    }
}

public struct BrowserRecordingCaptions: Sendable, Equatable {
    public static let maximumCues = 3000
    public static let maximumBytes = 1_048_576
    public static let maximumDurationMilliseconds: UInt64 = 86_400_000
    public let cues: [BrowserRecordingCue]

    public init(cues: [BrowserRecordingCue], durationMilliseconds: UInt64? = nil) throws {
        guard cues.count <= Self.maximumCues else { throw BrowserRecordingCaptionError.limitExceeded }
        let duration = durationMilliseconds ?? Self.maximumDurationMilliseconds
        guard duration <= Self.maximumDurationMilliseconds else { throw BrowserRecordingCaptionError.invalidCue }
        var identities: Set<String> = []
        var previousStart: UInt64 = 0
        var encodedBytes = 8
        var encodedLines = 0
        for cue in cues {
            guard identities.insert(cue.id).inserted, cue.startMilliseconds >= previousStart, cue.endMilliseconds <= duration else {
                throw BrowserRecordingCaptionError.invalidCue
            }
            previousStart = cue.startMilliseconds
            encodedBytes += Self.escape(cue.text).utf8.count + 48
            encodedLines += 4 + cue.text.utf8.reduce(0) { $0 + ($1 == 10 ? 1 : 0) }
            guard encodedBytes <= Self.maximumBytes, encodedLines <= Self.maximumCues * 8 else { throw BrowserRecordingCaptionError.limitExceeded }
        }
        self.cues = cues
    }

    public func active(atMilliseconds time: UInt64) -> [BrowserRecordingCue] {
        cues.prefix { $0.startMilliseconds <= time }.filter { time < $0.endMilliseconds }
    }

    public func srt() -> String {
        cues.enumerated().map { index, cue in
            "\(index + 1)\n\(Self.timestamp(cue.startMilliseconds, separator: ",")) --> \(Self.timestamp(cue.endMilliseconds, separator: ","))\n\(cue.text)\n\n"
        }.joined()
    }

    public func webVTT() -> String {
        "WEBVTT\n\n" + cues.enumerated().map { index, cue in
            "\(index + 1)\n\(Self.timestamp(cue.startMilliseconds, separator: ".")) --> \(Self.timestamp(cue.endMilliseconds, separator: "."))\n\(Self.escape(cue.text))\n\n"
        }.joined()
    }

    public static func parseSRT(_ text: String, durationMilliseconds: UInt64? = nil) throws -> Self {
        guard text.utf8.count <= maximumBytes else { throw BrowserRecordingCaptionError.limitExceeded }
        var input = text.replacingOccurrences(of: "\r\n", with: "\n").replacingOccurrences(of: "\r", with: "\n")[...]
        if input.first == "\u{FEFF}" { input.removeFirst() }
        var cues: [BrowserRecordingCue] = []
        var identity: String?
        var timing: (UInt64, UInt64)?
        var lines: [String] = []
        var textBytes = 0
        var lineCount = 0

        func appendCue() throws {
            guard let identity, let timing, !lines.isEmpty else { throw BrowserRecordingCaptionError.invalidFormat }
            guard cues.count < maximumCues else { throw BrowserRecordingCaptionError.limitExceeded }
            cues.append(try BrowserRecordingCue(id: identity, text: lines.joined(separator: "\n"), startMilliseconds: timing.0, endMilliseconds: timing.1))
        }

        while !input.isEmpty {
            lineCount += 1
            guard lineCount <= maximumCues * 8 else { throw BrowserRecordingCaptionError.limitExceeded }
            let boundary = input.firstIndex(of: "\n") ?? input.endIndex
            let line = String(input[..<boundary])
            input = boundary == input.endIndex ? ""[...] : input[input.index(after: boundary)...]
            if line.trimmingCharacters(in: .whitespaces).isEmpty {
                if identity != nil {
                    try appendCue()
                    identity = nil
                    timing = nil
                    lines = []
                    textBytes = 0
                }
            } else if identity == nil {
                guard line.utf8.count <= 10, line.utf8.allSatisfy({ (48...57).contains($0) }), let index = UInt32(line), index > 0, String(index) == line else {
                    throw BrowserRecordingCaptionError.invalidFormat
                }
                identity = line
            } else if timing == nil {
                let parts = line.components(separatedBy: " --> ")
                guard parts.count == 2 else { throw BrowserRecordingCaptionError.invalidFormat }
                timing = (try parseTimestamp(parts[0]), try parseTimestamp(parts[1]))
            } else {
                textBytes += line.utf8.count + (lines.isEmpty ? 0 : 1)
                guard textBytes <= 4096 else { throw BrowserRecordingCaptionError.limitExceeded }
                lines.append(line)
            }
        }
        if identity != nil { try appendCue() }
        return try Self(cues: cues, durationMilliseconds: durationMilliseconds)
    }

    private static func parseTimestamp(_ value: String) throws -> UInt64 {
        let bytes = Array(value.utf8)
        guard bytes.count == 12, bytes[2] == 58, bytes[5] == 58, bytes[8] == 44,
              [0, 1, 3, 4, 6, 7, 9, 10, 11].allSatisfy({ (48...57).contains(bytes[$0]) }) else {
            throw BrowserRecordingCaptionError.invalidFormat
        }
        let hours = UInt64(bytes[0] - 48) * 10 + UInt64(bytes[1] - 48)
        let minutes = UInt64(bytes[3] - 48) * 10 + UInt64(bytes[4] - 48)
        let seconds = UInt64(bytes[6] - 48) * 10 + UInt64(bytes[7] - 48)
        let milliseconds = UInt64(bytes[9] - 48) * 100 + UInt64(bytes[10] - 48) * 10 + UInt64(bytes[11] - 48)
        guard minutes < 60, seconds < 60 else { throw BrowserRecordingCaptionError.invalidFormat }
        let result = ((hours * 60 + minutes) * 60 + seconds) * 1000 + milliseconds
        guard result <= maximumDurationMilliseconds else { throw BrowserRecordingCaptionError.invalidFormat }
        return result
    }

    private static func timestamp(_ value: UInt64, separator: String) -> String {
        String(format: "%02llu:%02llu:%02llu%@%03llu", value / 3_600_000, value / 60_000 % 60, value / 1000 % 60, separator, value % 1000)
    }

    private static func escape(_ text: String) -> String {
        text.replacingOccurrences(of: "&", with: "&amp;").replacingOccurrences(of: "<", with: "&lt;").replacingOccurrences(of: ">", with: "&gt;")
    }
}
