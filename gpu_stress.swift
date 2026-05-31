// Saturates the Apple GPU with a Metal compute kernel for a given number of
// seconds, so the SMC mapping probe can see which sensors respond to GPU load.
// Usage: ./gpu_stress <seconds>

import Foundation
import Metal

let seconds = CommandLine.arguments.count > 1 ? (Double(CommandLine.arguments[1]) ?? 30.0) : 30.0

guard let device = MTLCreateSystemDefaultDevice(),
      let queue = device.makeCommandQueue() else {
    FileHandle.standardError.write("no Metal device\n".data(using: .utf8)!)
    exit(1)
}

let source = """
#include <metal_stdlib>
using namespace metal;
kernel void burn(device float* buf [[buffer(0)]], uint id [[thread_position_in_grid]]) {
    float x = buf[id] + 1.0f;
    for (int i = 0; i < 120000; i++) { x = fma(x, 1.0000001f, 0.0000001f); }
    buf[id] = x;
}
"""

let library = try! device.makeLibrary(source: source, options: nil)
let function = library.makeFunction(name: "burn")!
let pipeline = try! device.makeComputePipelineState(function: function)

let n = 1 << 20
let buffer = device.makeBuffer(length: n * MemoryLayout<Float>.stride,
                               options: .storageModeShared)!

let deadline = Date().addingTimeInterval(seconds)
var dispatches = 0
let width = pipeline.maxTotalThreadsPerThreadgroup
while Date() < deadline {
    let cb = queue.makeCommandBuffer()!
    let enc = cb.makeComputeCommandEncoder()!
    enc.setComputePipelineState(pipeline)
    enc.setBuffer(buffer, offset: 0, index: 0)
    enc.dispatchThreads(MTLSize(width: n, height: 1, depth: 1),
                        threadsPerThreadgroup: MTLSize(width: width, height: 1, depth: 1))
    enc.endEncoding()
    cb.commit()
    cb.waitUntilCompleted()
    dispatches += 1
}
print("gpu_stress done: \(dispatches) dispatches over \(seconds)s")
