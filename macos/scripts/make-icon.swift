import AppKit

guard CommandLine.arguments.count == 2 else {
    fatalError("Usage: swift make-icon.swift <output.iconset>")
}
let directory = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)

func render(pixels: Int, filename: String) throws {
    let bitmap = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: pixels, pixelsHigh: pixels,
                                  bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true,
                                  isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    let context = NSGraphicsContext(bitmapImageRep: bitmap)!
    NSGraphicsContext.current = context
    context.cgContext.scaleBy(x: CGFloat(pixels) / 1024, y: CGFloat(pixels) / 1024)
    // Orbit mark: three agents on one ring, white on black.
    NSColor(calibratedWhite: 0.04, alpha: 1).setFill()
    NSBezierPath(roundedRect: NSRect(x: 48, y: 48, width: 928, height: 928), xRadius: 205, yRadius: 205).fill()
    let center = NSPoint(x: 512, y: 512)
    let radius: CGFloat = 181
    NSColor.white.setStroke()
    let ring = NSBezierPath(ovalIn: NSRect(x: center.x - radius, y: center.y - radius, width: radius * 2, height: radius * 2))
    ring.lineWidth = 50
    ring.stroke()
    NSColor.white.setFill()
    for angle in [90.0, 210.0, 330.0] {
        let r = angle * .pi / 180
        let p = NSPoint(x: center.x + radius * CGFloat(cos(r)), y: center.y + radius * CGFloat(sin(r)))
        NSBezierPath(ovalIn: NSRect(x: p.x - 59, y: p.y - 59, width: 118, height: 118)).fill()
    }
    NSGraphicsContext.restoreGraphicsState()
    try bitmap.representation(using: .png, properties: [:])!.write(to: directory.appendingPathComponent(filename))
}

for size in [16, 32, 128, 256, 512] {
    try render(pixels: size, filename: "icon_\(size)x\(size).png")
    try render(pixels: size * 2, filename: "icon_\(size)x\(size)@2x.png")
}
