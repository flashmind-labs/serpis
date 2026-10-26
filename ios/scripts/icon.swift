// Draws the app icon (1024×1024, no transparency): the desktop's periwinkle as a gradient, and a
// chat bubble holding a prompt.
//   swift ios/scripts/icon.swift ios/Serpis/Assets.xcassets/AppIcon.appiconset/icon.png
import AppKit

let size = 1024.0
let out = CommandLine.arguments[1]
let space = CGColorSpaceCreateDeviceRGB()
let ctx = CGContext(data: nil, width: 1024, height: 1024, bitsPerComponent: 8, bytesPerRow: 0, space: space, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)!
let top = CGColor(red: 0x8B / 255, green: 0x9C / 255, blue: 0xFF / 255, alpha: 1)
let bottom = CGColor(red: 0x3E / 255, green: 0x4F / 255, blue: 0xC8 / 255, alpha: 1)
ctx.drawLinearGradient(CGGradient(colorsSpace: space, colors: [top, bottom] as CFArray, locations: [0, 1])!, start: CGPoint(x: 0, y: size), end: CGPoint(x: size * 0.35, y: 0), options: [.drawsBeforeStartLocation, .drawsAfterEndLocation])

// The bubble, with its tail at the lower left.
let bubble = CGRect(x: 212, y: 290, width: 600, height: 460)
let path = CGMutablePath()
path.addRoundedRect(in: bubble, cornerWidth: 150, cornerHeight: 150)
path.move(to: CGPoint(x: 290, y: 330))
path.addCurve(to: CGPoint(x: 220, y: 210), control1: CGPoint(x: 280, y: 270), control2: CGPoint(x: 250, y: 230))
path.addCurve(to: CGPoint(x: 400, y: 300), control1: CGPoint(x: 310, y: 225), control2: CGPoint(x: 370, y: 260))
path.closeSubpath()
ctx.setShadow(offset: CGSize(width: 0, height: -14), blur: 40, color: CGColor(red: 0.1, green: 0.12, blue: 0.4, alpha: 0.35))
ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
ctx.addPath(path)
ctx.fillPath()
ctx.setShadow(offset: .zero, blur: 0)

// The prompt: a chevron and a cursor.
ctx.setStrokeColor(bottom)
ctx.setLineWidth(56)
ctx.setLineCap(.round)
ctx.setLineJoin(.round)
ctx.move(to: CGPoint(x: 352, y: 610))
ctx.addLine(to: CGPoint(x: 462, y: 520))
ctx.addLine(to: CGPoint(x: 352, y: 430))
ctx.strokePath()
ctx.setStrokeColor(top)
ctx.move(to: CGPoint(x: 548, y: 430))
ctx.addLine(to: CGPoint(x: 662, y: 430))
ctx.strokePath()
try! NSBitmapImageRep(cgImage: ctx.makeImage()!).representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: out))
