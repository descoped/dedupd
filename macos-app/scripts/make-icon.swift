// Draws the app icon and writes it in every size the asset catalog lists.
//
//   swift macos-app/scripts/make-icon.swift macos-app/Dedupd/Assets.xcassets/AppIcon.appiconset
//
// The icon is the menu bar symbol, a stack of layers, in white on a blue
// rounded square: several apps, one copy underneath.
import AppKit

guard CommandLine.arguments.count == 2 else {
    FileHandle.standardError.write(Data("usage: make-icon.swift <AppIcon.appiconset>\n".utf8))
    exit(2)
}
let folder = URL(fileURLWithPath: CommandLine.arguments[1])

func icon(pixels: Int) -> Data? {
    guard let bitmap = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: pixels, pixelsHigh: pixels, bitsPerSample: 8, samplesPerPixel: 4,
        hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0
    ), let context = NSGraphicsContext(bitmapImageRep: bitmap) else {
        return nil
    }
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = context
    let size = CGFloat(pixels)

    // The shape macOS icons share: a rounded square inset from the canvas.
    let inset = size * 0.1
    let square = NSRect(x: inset, y: inset, width: size - 2 * inset, height: size - 2 * inset)
    let shape = NSBezierPath(roundedRect: square, xRadius: square.width * 0.225, yRadius: square.width * 0.225)
    let top = NSColor(calibratedRed: 0.29, green: 0.56, blue: 0.98, alpha: 1)
    let bottom = NSColor(calibratedRed: 0.11, green: 0.32, blue: 0.82, alpha: 1)
    NSGradient(starting: top, ending: bottom)?.draw(in: shape, angle: -90)

    let configuration = NSImage.SymbolConfiguration(pointSize: square.width * 0.5, weight: .medium)
        .applying(.init(paletteColors: [.white]))
    if let symbol = NSImage(systemSymbolName: "square.stack.3d.up.fill", accessibilityDescription: nil)?
        .withSymbolConfiguration(configuration) {
        let origin = NSPoint(x: square.midX - symbol.size.width / 2, y: square.midY - symbol.size.height / 2)
        symbol.draw(at: origin, from: .zero, operation: .sourceOver, fraction: 1)
    }
    NSGraphicsContext.restoreGraphicsState()
    return bitmap.representation(using: .png, properties: [:])
}

var images: [[String: String]] = []
for points in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let name = "icon-\(points)@\(scale)x.png"
        guard let data = icon(pixels: points * scale) else {
            FileHandle.standardError.write(Data("could not draw \(name)\n".utf8))
            exit(1)
        }
        try data.write(to: folder.appendingPathComponent(name))
        images.append(["idiom": "mac", "scale": "\(scale)x", "size": "\(points)x\(points)", "filename": name])
    }
}
let contents: [String: Any] = ["images": images, "info": ["author": "xcode", "version": 1]]
let json = try JSONSerialization.data(withJSONObject: contents, options: [.prettyPrinted, .sortedKeys])
try json.write(to: folder.appendingPathComponent("Contents.json"))
print("wrote \(images.count) images to \(folder.path)")
