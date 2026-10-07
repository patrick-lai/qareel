import Foundation
import WebKit

enum NativeBrowserConsoleAction: Decodable {
    case read
    case configure(NativeBrowserAutomationBinding, Bool)

    private enum Keys: String, CodingKey { case kind, binding, enabled }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Keys.self)
        switch try container.decode(String.self, forKey: .kind) {
        case "read": self = .read
        case "configure": self = .configure(try container.decode(NativeBrowserAutomationBinding.self, forKey: .binding), try container.decode(Bool.self, forKey: .enabled))
        default: throw NativeBrowserFailure(message: "browser.console_invalid: unknown console action")
        }
    }
}

@MainActor
final class NativeBrowserConsole {
    private static let prefix = "(()=>{const commissionConsoleHook=1;"
    private unowned let page: NativeBrowserPage
    private var origin: String?
    private var key: String?

    private struct Payload: Decodable {
        struct Entry: Decodable { let sequence: UInt32; let level: String; let text: String; let time_ms: UInt64 }
        let document_id: String
        let coverage: String
        let entries: [Entry]
        let dropped: UInt32
    }

    init(page: NativeBrowserPage) { self.page = page }

    static func owns(_ script: WKUserScript) -> Bool { script.source.hasPrefix(prefix) }

    private var binding: NativeBrowserAutomationBinding {
        .init(generation: page.hostGeneration ?? "", navigation_epoch: page.navigationEpoch, control_epoch: page.controlEpoch)
    }

    private static func pageOrigin(_ url: URL?) -> String? {
        guard let url, let parts = URLComponents(url: url, resolvingAgainstBaseURL: false), let scheme = parts.scheme?.lowercased(), ["http", "https"].contains(scheme), let host = parts.host?.lowercased() else { return nil }
        let port = parts.port.flatMap { ($0 == 80 && scheme == "http") || ($0 == 443 && scheme == "https") ? nil : $0 }
        return "\(scheme)://\(host)" + (port.map { ":\($0)" } ?? "")
    }

    private func state(_ status: String, reason: String? = nil, payload: JSONValue? = nil) -> JSONValue {
        .object(["status": .string(status), "reason": reason.map(JSONValue.string) ?? .null, "binding": binding.value, "document_id": payload?["document_id"] ?? .null, "coverage": payload?["coverage"] ?? .null, "entries": payload?["entries"] ?? .array([]), "dropped": payload?["dropped"] ?? .number(0)])
    }

    private func removeScript() {
        let controller = page.webView.configuration.userContentController
        let retained = controller.userScripts.filter { !Self.owns($0) }
        controller.removeAllUserScripts()
        for script in retained { controller.addUserScript(script) }
    }

    func invalidate() {
        removeScript()
        if let key {
            let encoded = String(decoding: JSONValue.string(key).encoded(), as: UTF8.self)
            page.webView.evaluateJavaScript("globalThis[\(encoded)]?.stop?.()", in: nil, in: .page, completionHandler: nil)
        }
        key = nil
        origin = nil
    }

    func navigation(to url: URL?) {
        if let origin, Self.pageOrigin(url) != origin { invalidate() }
    }

    func perform(_ action: NativeBrowserConsoleAction) async throws -> JSONValue {
        switch action {
        case .read: return try await read()
        case .configure(let expected, let enabled):
            guard expected == binding else { throw NativeBrowserFailure(message: "browser.console_stale: the displayed document or control identity changed") }
            if !enabled { invalidate(); return state("disabled") }
            guard !page.webView.isLoading, let currentOrigin = Self.pageOrigin(page.webView.url) else { return state("unavailable", reason: "Wait for an HTTP or HTTPS document to finish loading before enabling console capture") }
            if key != nil, origin == currentOrigin {
                let existing = try await read()
                if existing["status"] == "active" { return existing }
                guard expected == binding else { throw NativeBrowserFailure(message: "browser.console_stale: the document changed while enabling console capture") }
            }
            invalidate()
            let newKey = "__commission_console_" + UUID().uuidString.replacingOccurrences(of: "-", with: "")
            key = newKey
            origin = currentOrigin
            let source = Self.script(key: newKey, origin: currentOrigin, coverage: "document_start")
            page.webView.configuration.userContentController.addUserScript(WKUserScript(source: source, injectionTime: .atDocumentStart, forMainFrameOnly: true, in: .page))
            do {
                _ = try await page.evaluate(Self.script(key: newKey, origin: currentOrigin, coverage: "since_enable"))
                guard expected == binding else { throw NativeBrowserFailure(message: "browser.console_stale: the document changed while enabling console capture") }
                return try await read()
            } catch { invalidate(); throw error }
        }
    }

    private func read() async throws -> JSONValue {
        guard let key, let origin else { return state("disabled") }
        guard Self.pageOrigin(page.webView.url) == origin else { invalidate(); return state("disabled") }
        guard !page.webView.isLoading else { return state("unavailable", reason: "Navigation is in progress; previous document logs are not returned") }
        let before = binding
        let result = try await page.evaluate("globalThis[\(String(decoding: JSONValue.string(key).encoded(), as: UTF8.self))]?.read?.() ?? null")
        guard before == binding else { throw NativeBrowserFailure(message: "browser.console_stale: the document changed while reading console capture") }
        guard let text = result.stringValue, text.utf8.count <= 131072, let data = text.data(using: .utf8), let payload = try? JSONDecoder().decode(Payload.self, from: data), let value = JSONValue.parse(data), ["since_enable", "document_start"].contains(payload.coverage), payload.document_id.utf8.count <= 128, !payload.document_id.isEmpty, payload.entries.count <= 200 else { return state("unavailable", reason: "Console capture was replaced, disabled or returned invalid data") }
        var bytes = 0
        var previous: UInt32 = 0
        for entry in payload.entries {
            bytes += entry.text.utf8.count
            guard entry.sequence > previous, ["log", "info", "warn", "error", "debug", "page_error", "unhandled_rejection"].contains(entry.level), entry.text.utf8.count <= 2048, entry.time_ms <= 86400000 else { return state("unavailable", reason: "Console capture returned invalid entries") }
            previous = entry.sequence
        }
        guard bytes <= 65536 else { return state("unavailable", reason: "Console capture exceeded its retained text limit") }
        return state("active", payload: value)
    }

    static func script(key: String, origin: String, coverage: String) -> String {
        let key = String(decoding: JSONValue.string(key).encoded(), as: UTF8.self)
        let origin = String(decoding: JSONValue.string(origin).encoded(), as: UTF8.self)
        let coverage = String(decoding: JSONValue.string(coverage).encoded(), as: UTF8.self)
        return prefix + """
        const key=\(key),origin=\(origin),coverage=\(coverage);
        if(location.origin!==origin)return false;
        if(globalThis[key])return true;
        const clock=performance.now.bind(performance),started=clock(),apply=Reflect.apply,stringify=JSON.stringify;
        const rows=[],methods=[],consoleObject=globalThis.console;
        const random=new Uint32Array(4);crypto.getRandomValues(random);
        const document_id=Array.from(random,x=>x.toString(16)).join('-');
        let enabled=true,sequence=0,dropped=0,bytes=0,windowStart=started,windowCount=0;
        const drop=()=>{dropped=Math.min(4294967295,dropped+1)};
        const primitive=value=>{switch(typeof value){case 'string':return value.slice(0,512);case 'number':case 'boolean':return ''+value;case 'undefined':return 'undefined';case 'bigint':return '[bigint]';case 'symbol':return '[symbol]';case 'function':return '[function]';default:return value===null?'null':'[object]'}};
        const add=(level,args)=>{
          if(!enabled)return;
          const now=clock();if(now-windowStart>=1000){windowStart=now;windowCount=0}
          if(++windowCount>100||sequence===4294967295){drop();return}
          let text='';for(let i=0;i<Math.min(args.length,8);i++){text+=(i?' ':'')+primitive(args[i]);if(text.length>=512)break}text=text.slice(0,512);
          const cost=text.length*4;
          while(rows.length>=200||bytes+cost>65536){const old=rows.shift();bytes-=old.text.length*4;drop()}
          rows.push({sequence:++sequence,level,text,time_ms:Math.min(86400000,Math.max(0,Math.floor(now-started)))});bytes+=cost;
        };
        for(const level of ['log','info','warn','error','debug']){
          try{const original=consoleObject[level];if(typeof original!=='function')continue;
          const wrapper=function(...args){const result=apply(original,this,args);try{add(level,args)}catch{}return result};
          consoleObject[level]=wrapper;if(consoleObject[level]===wrapper)methods.push({level,original,wrapper})}catch{}
        }
        const onError=event=>{try{if(typeof event.message==='string')add('page_error',[event.message]);else add('page_error',['Resource error'])}catch{}};
        const onRejection=event=>{try{add('unhandled_rejection',[event.reason])}catch{}};
        addEventListener('error',onError);addEventListener('unhandledrejection',onRejection);
        const stop=()=>{enabled=false;rows.length=0;bytes=0;removeEventListener('error',onError);removeEventListener('unhandledrejection',onRejection);for(const item of methods){try{if(consoleObject[item.level]===item.wrapper)consoleObject[item.level]=item.original}catch{}}try{delete globalThis[key]}catch{}};
        const read=()=>{try{if(!enabled||methods.length!==5||globalThis.console!==consoleObject||methods.some(item=>consoleObject[item.level]!==item.wrapper))return null;return stringify({document_id,coverage,entries:rows,dropped})}catch{return null}};
        try{Object.defineProperty(globalThis,key,{value:Object.freeze({read,stop}),configurable:true})}catch(error){stop();throw error}
        return true;
        })()
        """
    }
}
