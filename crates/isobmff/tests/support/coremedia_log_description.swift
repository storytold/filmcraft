// macOS public-API wire-format cross-check; no media files are written.
import Foundation
import CoreMedia
var format: CMVideoFormatDescription?
let extensions: [String: Any] = [kCMFormatDescriptionExtension_LogTransferFunction as String: kCMFormatDescriptionLogTransferFunction_AppleLog]
let status = CMVideoFormatDescriptionCreate(allocator: kCFAllocatorDefault, codecType: kCMVideoCodecType_AppleProRes422, width: 64, height: 64, extensions: extensions as CFDictionary, formatDescriptionOut: &format)
print("create", status)
if let format {
  var block: CMBlockBuffer?
  let result = CMVideoFormatDescriptionCopyAsBigEndianImageDescriptionBlockBuffer(allocator: kCFAllocatorDefault, videoFormatDescription: format, stringEncoding: CFStringBuiltInEncodings.UTF8.rawValue, flavor: CMImageDescriptionFlavor.quickTimeMovie, blockBufferOut: &block)
  print("serialize", result)
  if let block {
    var data = [UInt8](repeating: 0, count: CMBlockBufferGetDataLength(block))
    let copied = CMBlockBufferCopyDataBytes(block, atOffset: 0, dataLength: data.count, destination: &data)
    print("copy",copied)
    print(data.map { String(format:"%02x", $0) }.joined())
    print(String(decoding: data, as: UTF8.self))
  }
}
