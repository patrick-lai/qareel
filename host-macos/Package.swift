// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "QareelHost",
    platforms: [
        .macOS(.v14)
    ],
    products: [
        .executable(name: "qareel-host", targets: ["QareelHost"]),
        .library(name: "QareelEngine", type: .dynamic, targets: ["QareelEngine"])
    ],
    targets: [
        .target(
            name: "QareelEngine",
            path: "Sources/QareelEngine",
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),
        .executableTarget(
            name: "QareelHost",
            dependencies: ["QareelEngine"],
            path: "Sources/QareelHost",
            swiftSettings: [.swiftLanguageMode(.v6)]
        )
    ]
)
