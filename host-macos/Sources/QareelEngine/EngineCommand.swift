import Foundation

struct NativeBrowserCommand: Decodable {
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
        case control(Bool)
        case automationObserve(String)
        case automationExecute(NativeBrowserAutomationBinding, String, NativeBrowserAutomationAction)
        case resize(Int?, Int?)
        case recording(String, JSONValue)
        case console(NativeBrowserConsoleAction)
        case popupDecision(String, UInt64, String, String, PopupDecision)
        case pointer(String, Double, Double)
        case tap(Double, Double)
        case hold(Int, String?)
        case key(String, String)
        case wheel(Double, Double, Double, Double, Bool)
        case frames
        case frameEvaluate(String, String)
        case dialog(JSONValue)
        case cookiesExport(Int, Int)
        case cookiesApply(JSONValue)

        enum PopupDecision: String { case admit, reject, closed }

        private enum Keys: String, CodingKey { case binding, action, width, height, sequence }

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
            case "control":
                guard let human = value["human"]?.boolValue else { throw NativeBrowserFailure(message: "browser.protocol_invalid: missing human control flag") }
                self = .control(human)
            case "recording_start", "recording_caption", "recording_stop", "recording_status", "recording_read", "recording_release":
                guard let kind = value["kind"]?.stringValue, let id = value["recording_id"]?.stringValue, UUID(uuidString: id)?.uuidString.lowercased() == id else { throw NativeBrowserFailure(message: "browser.recording_id_invalid") }
                self = .recording(kind, value)
            case "popup_decision":
                guard let decision = value["decision"]?.stringValue.flatMap(PopupDecision.init(rawValue:)),
                      let popup = UUID(uuidString: try required("popup_id", limit: 36)),
                      let instance = UUID(uuidString: try required("instance_id", limit: 36)) else { throw NativeBrowserFailure(message: "browser.protocol_invalid: invalid popup decision") }
                let container = try decoder.container(keyedBy: Keys.self)
                self = .popupDecision(instance.uuidString.lowercased(), try container.decode(UInt64.self, forKey: .sequence), popup.uuidString.lowercased(), try required("opener_id", limit: 128), decision)
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
            case "cookies_export":
                guard let offset = value["offset"]?.doubleValue, let limit = value["limit"]?.doubleValue, offset >= 0, limit >= 1, limit <= 500 else { throw NativeBrowserFailure(message: "browser.protocol_invalid: invalid cookie page") }
                self = .cookiesExport(Int(offset), Int(limit))
            case "cookies_apply": self = .cookiesApply(value)
            default: throw NativeBrowserFailure(message: "browser.capability_unavailable: unknown native operation")
            }
        }

        var direct: Bool {
            switch self { case .popupDecision, .recording, .cookiesExport, .cookiesApply: true; default: false }
        }

        var manualMutation: Bool {
            switch self {
            case .navigate, .back, .forward, .reload, .click, .type, .press, .resize, .tap, .pointer, .key, .wheel: true
            default: false
            }
        }
    }
}
