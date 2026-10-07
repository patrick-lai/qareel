import Foundation
import WebKit

struct NativeBrowserAutomationBinding: Decodable, Equatable {
    let generation: String
    let navigation_epoch: UInt64
    let control_epoch: UInt64

    var value: JSONValue {
        .object(["generation": .string(generation), "navigation_epoch": .number(Double(navigation_epoch)), "control_epoch": .number(Double(control_epoch))])
    }
}

enum NativeBrowserAutomationAction: Decodable {
    case click(String)
    case type(String, String)
    case select(String, String)
    case scrollUp, scrollDown, back, reload, wait

    private enum Keys: String, CodingKey { case operation, target, text, option }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Keys.self)
        func text(_ key: Keys, limit: Int = 256) throws -> String {
            let value = try container.decode(String.self, forKey: key)
            guard value.utf8.count <= limit else { throw NativeBrowserFailure(message: "browser.protocol_invalid: oversized runner action") }
            return value
        }
        switch try text(.operation) {
        case "CLICK": self = .click(try text(.target))
        case "TYPE_TEXT": self = .type(try text(.target), try text(.text, limit: 65536))
        case "SELECT": self = .select(try text(.target), try text(.option))
        case "SCROLL_UP": self = .scrollUp
        case "SCROLL_DOWN": self = .scrollDown
        case "BACK": self = .back
        case "RELOAD": self = .reload
        case "WAIT": self = .wait
        default: throw NativeBrowserFailure(message: "browser.protocol_invalid: unknown runner action")
        }
    }
}

@MainActor
final class NativeBrowserAutomation {
    private static let world = WKContentWorld.world(name: "commission-browser-agent")
    private unowned let page: NativeBrowserPage
    private var observation: Observation?

    private struct Observation {
        let binding: NativeBrowserAutomationBinding
        let token: String
        let url: URL?
        let size: CGSize
        let zoom: Double
        let magnification: Double
    }

    init(page: NativeBrowserPage) { self.page = page }

    func invalidate() { observation = nil }

    private func binding(_ generation: String) -> NativeBrowserAutomationBinding {
        NativeBrowserAutomationBinding(generation: generation, navigation_epoch: page.navigationEpoch, control_epoch: page.controlEpoch)
    }

    private func validate(_ snapshot: Observation, epoch: UInt64, deadline: Double, generation: String) throws {
        try page.validate(agent: true, epoch: epoch, deadline: deadline, generation: generation)
        guard snapshot.binding == binding(generation), snapshot.url == page.webView.url,
              snapshot.size == page.webView.bounds.size, snapshot.zoom == page.webView.pageZoom, snapshot.magnification == page.webView.magnification else {
            throw NativeBrowserFailure(message: "browser.stale: native observation changed before input")
        }
    }

    func observe(bootstrap: String, epoch: UInt64, deadline: Double, generation: String) async throws -> JSONValue {
        invalidate()
        try page.validate(agent: true, epoch: epoch, deadline: deadline, generation: generation)
        guard bootstrap.utf8.count <= 65536 else { throw NativeBrowserFailure(message: "browser.protocol_invalid: runner bootstrap exceeds 64 KiB") }
        let captured = Observation(binding: binding(generation), token: "", url: page.webView.url, size: page.webView.bounds.size, zoom: page.webView.pageZoom, magnification: page.webView.magnification)
        let value = try await page.evaluate("(() => { \(bootstrap); return globalThis.__commissionFastBrowser.observe(); })()", world: Self.world)
        try validate(captured, epoch: epoch, deadline: deadline, generation: generation)
        guard value.encoded().count <= 256 * 1024, let token = value["token"]?.stringValue, token.utf8.count <= 256 else {
            throw NativeBrowserFailure(message: "browser.observation_limit: invalid or oversized native observation")
        }
        observation = Observation(binding: captured.binding, token: token, url: captured.url, size: captured.size, zoom: captured.zoom, magnification: captured.magnification)
        return .object(["binding": captured.binding.value, "observation": value])
    }

    private func call(_ method: String, _ arguments: [JSONValue], deadline: Double) async throws -> JSONValue {
        let args = String(decoding: JSONValue.array(arguments).encoded(), as: UTF8.self)
        do {
            return try await page.evaluate("(() => { if (Date.now() > \(deadline)) return {ok:false,error:'stale'}; return globalThis.__commissionFastBrowser?.\(method)?.(...\(args)) ?? {ok:false,error:'stale'}; })()", world: Self.world)
        } catch {
            if error.localizedDescription.contains("browser.process_unresponsive") { throw error }
            throw NativeBrowserFailure(message: "browser.stale: native runner context is unavailable")
        }
    }

    private func accepted(_ value: JSONValue) throws {
        guard value["ok"]?.boolValue == true else {
            let code = value["error"]?.stringValue ?? "stale"
            let reason = ["stale", "covered", "disabled", "unsupported"].contains(code) ? code : "stale"
            throw NativeBrowserFailure(message: "browser.stale: \(reason); runner target changed or no longer supports this action")
        }
    }

    private func armHover(_ point: JSONValue, deadline: Double) async throws {
        let coordinates = String(decoding: point.encoded(), as: UTF8.self)
        _ = try await page.evaluate("""
        (() => {
          globalThis.__commissionNativeHover?.cancel();
          const point = \(coordinates);
          let finish;
          const promise = new Promise(resolve => { finish = resolve; });
          let timer;
          const complete = ok => {
            clearTimeout(timer);
            removeEventListener('mousemove', moved, true);
            finish(ok);
          };
          const moved = event => {
            if (event.isTrusted && Math.abs(event.clientX - point.x) <= 1 && Math.abs(event.clientY - point.y) <= 1) complete(true);
          };
          addEventListener('mousemove', moved, true);
          timer = setTimeout(() => complete(false), Math.max(0, Math.min(1000, \(deadline) - Date.now())));
          globalThis.__commissionNativeHover = {promise, cancel: () => complete(false)};
          return true;
        })()
        """, world: Self.world)
    }

    private func inputBoundary(epoch: UInt64, deadline: Double, generation: String) async throws {
        do {
            _ = try await page.evaluate("true", world: Self.world)
            try page.validate(agent: true, epoch: epoch, deadline: deadline, generation: generation)
        } catch {
            let reason = error.localizedDescription.contains("browser.process_unresponsive") ? "browser.process_unresponsive" : "native input acknowledgement failed"
            throw NativeBrowserFailure(message: "browser.action_uncertain: \(reason); input was already issued")
        }
    }

    func execute(binding requested: NativeBrowserAutomationBinding, token: String, action: NativeBrowserAutomationAction, epoch: UInt64, deadline: Double, generation: String) async throws {
        guard let snapshot = observation, snapshot.binding == requested, snapshot.token == token else {
            throw NativeBrowserFailure(message: "browser.stale: native observation expired or was already consumed")
        }
        observation = nil
        try validate(snapshot, epoch: epoch, deadline: deadline, generation: generation)
        func current() throws { try validate(snapshot, epoch: epoch, deadline: deadline, generation: generation) }
        func delivered() throws {
            do { try page.validate(agent: true, epoch: epoch, deadline: deadline, generation: generation) }
            catch { throw NativeBrowserFailure(message: "browser.action_uncertain: native input was issued before control or connection changed") }
        }
        switch action {
        case .click(let target):
            try page.validatePointerScale()
            let prepared = try await call("prepare", [.string(token), "click", .string(target), ""], deadline: deadline)
            try current()
            try accepted(prepared)
            try await armHover(prepared, deadline: deadline)
            try current()
            do { try page.pointer(prepared, click: false) }
            catch { throw NativeBrowserFailure(message: "browser.stale: native hover could not be delivered") }
            let hovered = try await page.evaluate("globalThis.__commissionNativeHover?.promise ?? false", world: Self.world)
            try current()
            guard hovered.boolValue == true else { throw NativeBrowserFailure(message: "browser.stale: native hover was not acknowledged; no click was sent") }
            let point = try await call("clickCurrent", [.string(target)], deadline: deadline)
            try current()
            try accepted(point)
            do { try page.pointer(point, click: true) }
            catch { throw NativeBrowserFailure(message: "browser.stale: native click target is unavailable") }
            try await inputBoundary(epoch: epoch, deadline: deadline, generation: generation)
        case .type(let target, let text):
            let prepared = try await call("prepare", [.string(token), "type", .string(target), ""], deadline: deadline)
            try current()
            try accepted(prepared)
            page.webView.window?.makeFirstResponder(page.webView)
            let focused = try await call("focus", [.string(target)], deadline: deadline)
            try current()
            try accepted(focused)
            let checked = try await call("focusCurrent", [.string(target)], deadline: deadline)
            try current()
            try accepted(checked)
            page.recordAutomationTarget(checked)
            do { try page.insertPreparedText(text) }
            catch { throw NativeBrowserFailure(message: "browser.action_uncertain: native text delivery could not be confirmed") }
            try await inputBoundary(epoch: epoch, deadline: deadline, generation: generation)
        case .select(let target, let option):
            let prepared = try await call("prepare", [.string(token), "select", .string(target), .string(option)], deadline: deadline)
            try current()
            try accepted(prepared)
            page.recordAutomationTarget(prepared)
            let selected: JSONValue
            do { selected = try await call("select", [.string(target), .string(option)], deadline: deadline) }
            catch {
                let marker = error.localizedDescription.contains("browser.process_unresponsive") ? "browser.process_unresponsive: " : ""
                throw NativeBrowserFailure(message: "browser.action_uncertain: \(marker)native selection delivery could not be confirmed")
            }
            try accepted(selected)
            try delivered()
        case .scrollUp, .scrollDown:
            let direction: JSONValue
            if case .scrollUp = action { direction = "up" } else { direction = "down" }
            let scrolled: JSONValue
            do { scrolled = try await call("scroll", [direction], deadline: deadline) }
            catch {
                let marker = error.localizedDescription.contains("browser.process_unresponsive") ? "browser.process_unresponsive: " : ""
                throw NativeBrowserFailure(message: "browser.action_uncertain: \(marker)native scroll delivery could not be confirmed")
            }
            try accepted(scrolled)
            try delivered()
        case .back:
            guard page.webView.canGoBack else { throw NativeBrowserFailure(message: "browser.stale: no previous page exists") }
            guard page.webView.goBack() != nil else { throw NativeBrowserFailure(message: "browser.action_uncertain: history navigation delivery could not be confirmed") }
            try delivered()
        case .reload:
            guard page.webView.reloadFromOrigin() != nil else { throw NativeBrowserFailure(message: "browser.action_uncertain: reload delivery could not be confirmed") }
            try delivered()
        case .wait: break
        }
    }
}
