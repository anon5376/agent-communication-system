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
    NSColor(calibratedRed: 0.26, green: 0.28, blue: 0.67, alpha: 1).setFill()
    NSBezierPath(roundedRect: NSRect(x: 48, y: 48, width: 928, height: 928), xRadius: 205, yRadius: 205).fill()
    let top = NSPoint(x: 512, y: 756)
    let left = NSPoint(x: 276, y: 306)
    let right = NSPoint(x: 748, y: 306)
    let center = NSPoint(x: 512, y: 468)
    NSColor.white.withAlphaComponent(0.65).setStroke()
    for (a, b) in [(top, left), (left, right), (right, top), (top, center), (left, center), (right, center)] {
        let line = NSBezierPath()
        line.move(to: a)
        line.line(to: b)
        line.lineWidth = 24
        line.lineCapStyle = .round
        line.stroke()
    }
    NSColor.white.setFill()
    for point in [top, left, right] {
        NSBezierPath(ovalIn: NSRect(x: point.x - 56, y: point.y - 56, width: 112, height: 112)).fill()
    }
    NSColor(calibratedRed: 0.74, green: 0.79, blue: 1, alpha: 1).setFill()
    NSBezierPath(ovalIn: NSRect(x: center.x - 38, y: center.y - 38, width: 76, height: 76)).fill()
    NSGraphicsContext.restoreGraphicsState()
    try bitmap.representation(using: .png, properties: [:])!.write(to: directory.appendingPathComponent(filename))
}

for size in [16, 32, 128, 256, 512] {
    try render(pixels: size, filename: "icon_\(size)x\(size).png")
    try render(pixels: size * 2, filename: "icon_\(size)x\(size)@2x.png")
}
