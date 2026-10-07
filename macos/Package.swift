// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "ACS",
    platforms: [
        .macOS(.v14)
    ],
    products: [
        .library(name: "ACSCore", targets: ["ACSCore"]),
        .executable(name: "ACS", targets: ["ACS"])
    ],
    targets: [
        .target(
            name: "ACSCore",
            path: "Sources/ACSCore"
        ),
        .executableTarget(
            name: "ACS",
            dependencies: ["ACSCore"],
            path: "Sources/ACS"
        ),
        .testTarget(
            name: "ACSCoreTests",
            dependencies: ["ACSCore"],
            path: "Tests/ACSCoreTests"
        )
    ]
)
