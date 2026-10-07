// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "QareelHost",
    platforms: [
        .macOS(.v14)
    ],
    products: [
        .executable(name: "qareel-host", targets: ["QareelHost"])
    ],
    targets: [
        .executableTarget(
            name: "QareelHost",
            path: "Sources/QareelHost",
            swiftSettings: [.swiftLanguageMode(.v6)]
        )
    ]
)
