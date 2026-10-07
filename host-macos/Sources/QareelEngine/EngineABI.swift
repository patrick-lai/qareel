import AppKit
import WebKit
import Darwin

public typealias QareelEngineCallback = @convention(c) (UnsafeMutableRawPointer?, Int32, Int32, UnsafePointer<CChar>?) -> Void

private func text(_ pointer: UnsafePointer<CChar>?) -> String? {
    pointer.map { String(cString: $0) }
}

private func object(_ text: String?) -> JSONValue? {
    text.flatMap { JSONValue.parse(Data($0.utf8)) }
}

@MainActor
private func engine(_ address: UInt) -> QareelEngine? {
    UnsafeMutableRawPointer(bitPattern: address).map { Unmanaged<QareelEngine>.fromOpaque($0).takeUnretainedValue() }
}

@_cdecl("qareel_engine_abi")
public func qareelEngineABI() -> Int32 {
    QareelEngine.abi
}

@_cdecl("qareel_engine_version")
public func qareelEngineVersion() -> UnsafePointer<CChar> {
    UnsafeRawPointer(QareelEngine.version.utf8Start).assumingMemoryBound(to: CChar.self)
}

@_cdecl("qareel_engine_create")
public func qareelEngineCreate(_ configuration: UnsafePointer<CChar>?, _ store: UnsafeMutableRawPointer?, _ callback: QareelEngineCallback?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutableRawPointer? {
    guard let callback, let value = object(text(configuration)) else { return nil }
    let storeAddress = UInt(bitPattern: store)
    let contextAddress = UInt(bitPattern: context)
    let address: UInt = MainActor.assumeIsolated {
        guard let profile = value["profile_id"]?.stringValue.flatMap(UUID.init(uuidString:)), let recordings = value["recordings_dir"]?.stringValue, recordings.hasPrefix("/") else { return 0 }
        let dataStore = UnsafeMutableRawPointer(bitPattern: storeAddress).map { Unmanaged<WKWebsiteDataStore>.fromOpaque($0).takeUnretainedValue() } ?? WKWebsiteDataStore(forIdentifier: profile)
        let limit = value["page_limit"]?.doubleValue.map { max(1, min(64, Int($0))) } ?? 16
        let presentation = value["presentation"]?.stringValue == "embedded_mac" ? "embedded_mac" : "none"
        var extra: [String] = []
        if case .array(let items)? = value["extra_operations"] { extra = items.compactMap(\.stringValue) }
        let created = QareelEngine(profileID: profile, store: dataStore, recordings: URL(fileURLWithPath: recordings, isDirectory: true), pageLimit: limit, presentation: presentation, extraOperations: extra) { scope, channel, data in
            var bytes = data
            bytes.append(0)
            bytes.withUnsafeBytes { raw in callback(UnsafeMutableRawPointer(bitPattern: contextAddress), scope, channel, raw.baseAddress?.assumingMemoryBound(to: CChar.self)) }
        }
        return UInt(bitPattern: Unmanaged.passRetained(created).toOpaque())
    }
    return UnsafeMutableRawPointer(bitPattern: address)
}

@_cdecl("qareel_engine_destroy")
public func qareelEngineDestroy(_ pointer: UnsafeMutableRawPointer?) {
    let address = UInt(bitPattern: pointer)
    MainActor.assumeIsolated {
        guard let pointer = UnsafeMutableRawPointer(bitPattern: address) else { return }
        let released = Unmanaged<QareelEngine>.fromOpaque(pointer)
        released.takeUnretainedValue().interruptAll(reason: "Native host stopped")
        released.release()
    }
}

@_cdecl("qareel_engine_open_scope")
public func qareelEngineOpenScope(_ pointer: UnsafeMutableRawPointer?, _ configuration: UnsafePointer<CChar>?) -> Int32 {
    let address = UInt(bitPattern: pointer)
    guard let value = object(text(configuration)), let id = value["scope_id"]?.stringValue, !id.isEmpty, id.utf8.count <= 128 else { return -1 }
    return MainActor.assumeIsolated {
        engine(address)?.openScope(id: id, remote: value["remote"]?.boolValue ?? false, recordingOwner: value["recording_owner"]?.stringValue, popups: value["popups"]?.boolValue ?? false) ?? -1
    }
}

@_cdecl("qareel_engine_submit")
public func qareelEngineSubmit(_ pointer: UnsafeMutableRawPointer?, _ scope: Int32, _ message: UnsafePointer<CChar>?) {
    let address = UInt(bitPattern: pointer)
    guard let message = text(message) else { return }
    MainActor.assumeIsolated { engine(address)?.submit(scope: scope, data: Data(message.utf8)) }
}

@_cdecl("qareel_engine_disconnect")
public func qareelEngineDisconnect(_ pointer: UnsafeMutableRawPointer?, _ scope: Int32) {
    let address = UInt(bitPattern: pointer)
    MainActor.assumeIsolated { engine(address)?.disconnect(scope: scope) }
}

@_cdecl("qareel_engine_close_scope")
public func qareelEngineCloseScope(_ pointer: UnsafeMutableRawPointer?, _ scope: Int32) {
    let address = UInt(bitPattern: pointer)
    MainActor.assumeIsolated { engine(address)?.closeScope(scope) }
}

@_cdecl("qareel_engine_view")
public func qareelEngineView(_ pointer: UnsafeMutableRawPointer?, _ scope: Int32, _ tab: UnsafePointer<CChar>?) -> UnsafeMutableRawPointer? {
    let address = UInt(bitPattern: pointer)
    guard let tab = text(tab) else { return nil }
    let view: UInt = MainActor.assumeIsolated {
        engine(address)?.view(scope: scope, tab: tab).map { UInt(bitPattern: Unmanaged.passUnretained($0).toOpaque()) } ?? 0
    }
    return UnsafeMutableRawPointer(bitPattern: view)
}

@_cdecl("qareel_engine_query")
public func qareelEngineQuery(_ pointer: UnsafeMutableRawPointer?, _ scope: Int32, _ request: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>? {
    let address = UInt(bitPattern: pointer)
    guard let request = object(text(request)) else { return nil }
    let answer: String? = MainActor.assumeIsolated {
        engine(address).map { String(decoding: $0.query(scope: scope, request: request).encoded(), as: UTF8.self) }
    }
    return answer.flatMap { strdup($0) }
}

@_cdecl("qareel_engine_control")
public func qareelEngineControl(_ pointer: UnsafeMutableRawPointer?, _ scope: Int32, _ request: UnsafePointer<CChar>?) {
    let address = UInt(bitPattern: pointer)
    guard let request = object(text(request)) else { return }
    MainActor.assumeIsolated { engine(address)?.control(scope: scope, request: request) }
}

@_cdecl("qareel_engine_recording")
public func qareelEngineRecording(_ pointer: UnsafeMutableRawPointer?) -> Int32 {
    let address = UInt(bitPattern: pointer)
    return MainActor.assumeIsolated { engine(address)?.recording == true ? 1 : 0 }
}

@_cdecl("qareel_engine_free")
public func qareelEngineFree(_ pointer: UnsafeMutableRawPointer?) {
    free(pointer)
}
