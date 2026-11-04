// swift-tools-version: 5.9

import PackageDescription

let package = Package(
    name: "Sqarvo",
    platforms: [
        .macOS(.v14)
    ],
    targets: [
        .executableTarget(
            name: "SqarvoApp",
            path: "Sources/SqarvoApp"
        )
    ]
)
