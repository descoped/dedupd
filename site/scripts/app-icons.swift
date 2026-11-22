// Makes the site's icons from the app's own icon, as macOS draws it:
// app/icon.png (512 px), app/apple-icon.png (180 px, corners filled, for the
// home screen of an iPhone or iPad) and app/favicon.ico (16, 32 and 48 px).
//
//   swift scripts/app-icons.swift /Applications/dedupd.app app
//
// Run it again after the app icon (macos-app/Dedupd/AppIcon.icon) changes,
// with an app built from that icon.
import AppKit
import UniformTypeIdentifiers

let args = CommandLine.arguments
let out = URL(fileURLWithPath: args[2], isDirectory: true)
try FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)

// The icon at 1024 px, drawn by macOS from the app's compiled icon.
let w = 1024, h = 1024
let drawn = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: w, pixelsHigh: h, bitsPerSample: 8,
                             samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
                             bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: drawn)
NSWorkspace.shared.icon(forFile: args[1]).draw(in: NSRect(x: 0, y: 0, width: w, height: h))
NSGraphicsContext.restoreGraphicsState()
let image = drawn.cgImage!
// Read the pixels to find the rounded square: everything not transparent.
let space = CGColorSpace(name: CGColorSpace.sRGB)!
var px = [UInt8](repeating: 0, count: w * h * 4)
let read = CGContext(data: &px, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4, space: space,
                     bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
read.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
var (minX, minY, maxX, maxY) = (w, h, 0, 0)
for y in 0..<h { for x in 0..<w where px[(y * w + x) * 4 + 3] > 200 {
    minX = min(minX, x); maxX = max(maxX, x); minY = min(minY, y); maxY = max(maxY, y)
} }
let side = max(maxX - minX, maxY - minY) + 1
let crop = image.cropping(to: CGRect(x: minX, y: h - 1 - maxY, width: side, height: side))!
// The background inside a top corner, clear of the rim and the dial, fills the corners
// of the touch icon (iOS rounds them itself and shows black where it is clear).
let (bx, by) = (minX + side * 12 / 100, maxY - side * 12 / 100)
let i = (by * w + bx) * 4
let fill = CGColor(srgbRed: CGFloat(px[i]) / 255, green: CGFloat(px[i + 1]) / 255, blue: CGFloat(px[i + 2]) / 255, alpha: 1)
print("square \(side)px at \(minX),\(minY); fill \(px[i]),\(px[i + 1]),\(px[i + 2])")

func png(_ size: Int, filled: Bool) -> Data {
    let ctx = CGContext(data: nil, width: size, height: size, bitsPerComponent: 8, bytesPerRow: 0, space: space,
                        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.interpolationQuality = .high
    let all = CGRect(x: 0, y: 0, width: size, height: size)
    if filled { ctx.setFillColor(fill); ctx.fill(all) }
    ctx.draw(crop, in: all)
    let data = NSMutableData()
    let dest = CGImageDestinationCreateWithData(data, UTType.png.identifier as CFString, 1, nil)!
    CGImageDestinationAddImage(dest, ctx.makeImage()!, nil)
    CGImageDestinationFinalize(dest)
    return data as Data
}

try png(512, filled: false).write(to: out.appendingPathComponent("icon.png"))
try png(180, filled: true).write(to: out.appendingPathComponent("apple-icon.png"))

// favicon.ico: 16, 32 and 48 pixels, each entry a PNG (allowed since Vista).
let sizes = [16, 32, 48]
let images = sizes.map { png($0, filled: false) }
var ico = Data([0, 0, 1, 0, UInt8(sizes.count), 0])
var offset = 6 + 16 * sizes.count
func le16(_ v: Int) -> [UInt8] { [UInt8(v & 0xff), UInt8(v >> 8 & 0xff)] }
func le32(_ v: Int) -> [UInt8] { le16(v & 0xffff) + le16(v >> 16) }
for (size, data) in zip(sizes, images) {
    ico += Data([UInt8(size), UInt8(size), 0, 0] + le16(1) + le16(32) + le32(data.count) + le32(offset))
    offset += data.count
}
for data in images { ico += data }
try ico.write(to: out.appendingPathComponent("favicon.ico"))
