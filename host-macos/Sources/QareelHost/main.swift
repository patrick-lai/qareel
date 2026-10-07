import AppKit
import Darwin

signal(SIGPIPE, SIG_IGN)

let bootstrap: QareelBootstrap
let profileLock: Int32
do {
    bootstrap = try QareelBootstrap.read()
    profileLock = try bootstrap.lock()
} catch {
    StdioOutput.log(error.localizedDescription)
    exit(1)
}

MainActor.assumeIsolated {
    let application = NSApplication.shared
    application.setActivationPolicy(.prohibited)
    let host = StdioHost(bootstrap: bootstrap, lockFD: profileLock)
    host.start()
    application.run()
}
