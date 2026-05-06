/// CoreAudio process-tap audio streaming binary for Hush meeting mode.
///
/// Captures all system audio using `AudioHardwareCreateProcessTap` (macOS 14.2+)
/// and streams it as raw f32 LE interleaved PCM to stdout. Requires no Screen
/// Recording TCC permission — only the standard microphone-style audio consent
/// dialog (no pixels captured).
///
/// Protocol:
///   stdout: 12-byte header (on successful start) followed by continuous PCM.
///   Header layout (little-endian):
///     bytes 0–3:  b"HUSH" magic
///     bytes 4–7:  sample_rate as u32
///     bytes 8–11: channel_count as u32
///
///   stderr: human-readable diagnostic messages.
///   Termination: SIGTERM causes clean shutdown (tap + aggregate device destroyed).
///
/// Build (see src-tauri/build.rs::bundle_audio_tap_capture):
///   swiftc resources/macos-audio-tap.swift \
///       -framework CoreAudio -framework AudioToolbox \
///       -framework AVFAudio -framework Foundation \
///       -o src-tauri/resources/hush-audio-tap-capture

import AudioToolbox
import AVFoundation
import CoreAudio
import Foundation

guard #available(macOS 14.2, *) else {
    fputs("unsupported: macOS 14.2+ required\n", stderr)
    exit(2)
}

// ── 1. Default output device UID ─────────────────────────────────────────────

func defaultOutputDeviceUID() -> String? {
    var devID = AudioObjectID(kAudioObjectUnknown)
    var propSize = UInt32(MemoryLayout<AudioObjectID>.size)
    var addr = AudioObjectPropertyAddress(
        mSelector: kAudioHardwarePropertyDefaultOutputDevice,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain)
    guard AudioObjectGetPropertyData(
        AudioObjectID(kAudioObjectSystemObject), &addr, 0, nil, &propSize, &devID
    ) == noErr, devID != kAudioObjectUnknown else { return nil }

    var cfUID: Unmanaged<CFString>? = nil
    propSize = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
    var uidAddr = AudioObjectPropertyAddress(
        mSelector: kAudioDevicePropertyDeviceUID,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain)
    guard AudioObjectGetPropertyData(devID, &uidAddr, 0, nil, &propSize, &cfUID) == noErr,
          let uid = cfUID else { return nil }
    return uid.takeRetainedValue() as String
}

// ── 2. Process tap ───────────────────────────────────────────────────────────

let tapUUID = UUID()
let desc = CATapDescription()
desc.name = "hush-capture"
desc.uuid = tapUUID
desc.processes = []      // capture all system audio
desc.isMono = false      // stereo (downmixed by Whisper pipeline)
desc.isExclusive = false // non-exclusive — does not mute the tapped app
desc.isMixdown = true    // mix all process audio into one stream
desc.isPrivate = true    // don't expose tap as a public device
desc.muteBehavior = .unmuted

var tapID = AudioObjectID(kAudioObjectUnknown)
let tapStatus = AudioHardwareCreateProcessTap(desc, &tapID)
guard tapStatus == noErr else {
    fputs("error: tap creation failed: OSStatus=\(tapStatus)\n", stderr)
    exit(1)
}

// ── 3. Aggregate device with the tap ─────────────────────────────────────────

let tapUID = tapUUID.uuidString
let aggUID = "io.github.khawkins98.hush.capture-\(UUID().uuidString)"

// The tap-list entry uses the tap's UUID as its UID; the aggregate
// device wraps it so AVAudioEngine can address it like a real input.
var aggDesc: [String: Any] = [
    kAudioAggregateDeviceNameKey as String:      "HushCapture",
    kAudioAggregateDeviceUIDKey as String:       aggUID,
    kAudioAggregateDeviceIsPrivateKey as String: 1,
    kAudioAggregateDeviceIsStackedKey as String: 0,
    kAudioAggregateDeviceTapListKey as String:   [[kAudioSubTapUIDKey as String: tapUID]],
]
// Providing the default output device as main sub-device ties the
// aggregate device's clock to the system output clock — important
// for timestamp accuracy when mixing with microphone captures.
if let outputUID = defaultOutputDeviceUID() {
    aggDesc[kAudioAggregateDeviceMainSubDeviceKey as String] = outputUID
}

var aggDeviceID = AudioObjectID(kAudioObjectUnknown)
let aggStatus = AudioHardwareCreateAggregateDevice(aggDesc as CFDictionary, &aggDeviceID)
guard aggStatus == noErr else {
    AudioHardwareDestroyProcessTap(tapID)
    fputs("error: aggregate device creation failed: OSStatus=\(aggStatus)\n", stderr)
    exit(1)
}

// ── 4. AVAudioEngine pointing at aggregate device ─────────────────────────────

let engine = AVAudioEngine()
let inputNode = engine.inputNode
guard let audioUnit = inputNode.audioUnit else {
    AudioHardwareDestroyAggregateDevice(aggDeviceID)
    AudioHardwareDestroyProcessTap(tapID)
    fputs("error: no audioUnit on inputNode\n", stderr)
    exit(1)
}

var deviceIDForProperty = aggDeviceID
let setStatus = AudioUnitSetProperty(
    audioUnit,
    kAudioOutputUnitProperty_CurrentDevice,
    kAudioUnitScope_Global,
    0,
    &deviceIDForProperty,
    UInt32(MemoryLayout<AudioDeviceID>.size))
guard setStatus == noErr else {
    AudioHardwareDestroyAggregateDevice(aggDeviceID)
    AudioHardwareDestroyProcessTap(tapID)
    fputs("error: failed to set input device: OSStatus=\(setStatus)\n", stderr)
    exit(1)
}

// Prepare the engine so the input node's output format is resolved
// before we install the tap or query sample rate / channel count.
engine.prepare()
let captureFormat = inputNode.outputFormat(forBus: 0)
let sampleRate = UInt32(captureFormat.sampleRate)
let channelCount = captureFormat.channelCount

// ── 5. Header ─────────────────────────────────────────────────────────────────

// Write header BEFORE installing the tap so the Rust reader always
// sees the header first, with no risk of a race between the first
// PCM dispatch and the header write.
let stdout = FileHandle.standardOutput
var header = Data(count: 12)
header.withUnsafeMutableBytes { raw in
    let p = raw.baseAddress!.assumingMemoryBound(to: UInt8.self)
    // Magic
    p[0] = 0x48; p[1] = 0x55; p[2] = 0x53; p[3] = 0x48  // "HUSH"
    // sample_rate as u32 LE
    var sr = sampleRate.littleEndian
    withUnsafeBytes(of: &sr) { bytes in
        for i in 0..<4 { p[4 + i] = bytes[i] }
    }
    // channel_count as u32 LE
    var ch = channelCount.littleEndian
    withUnsafeBytes(of: &ch) { bytes in
        for i in 0..<4 { p[8 + i] = bytes[i] }
    }
}
stdout.write(header)

// ── 6. Audio tap callback ─────────────────────────────────────────────────────

// The write queue decouples stdout I/O from the real-time audio callback.
// A bounded semaphore (32 in-flight write slots) prevents unbounded queue
// growth when the Rust reader is slow — tryWait(timeout:.now()) drops the
// chunk rather than blocking the audio thread.
let writeQueue = DispatchQueue(label: "hush.audio.writer", qos: .userInteractive)
let semaphore = DispatchSemaphore(value: 32)

inputNode.installTap(onBus: 0, bufferSize: 2048, format: nil) { buffer, _ in
    guard let channelData = buffer.floatChannelData else { return }
    let frameCount = Int(buffer.frameLength)
    let chanCount = Int(buffer.format.channelCount)
    guard frameCount > 0, chanCount > 0 else { return }

    // Build interleaved f32 LE bytes on the audio thread. Allocation
    // cost is ~10–30 µs for 2048 frames — acceptable on the audio
    // thread since this path involves no system calls.
    var bytes = Data(count: frameCount * chanCount * 4)
    bytes.withUnsafeMutableBytes { raw in
        let ptr = raw.baseAddress!.assumingMemoryBound(to: Float.self)
        for frame in 0..<frameCount {
            for ch in 0..<chanCount {
                ptr[frame * chanCount + ch] = channelData[ch][frame]
            }
        }
    }

    // Non-blocking acquire: if the queue is saturated, drop this chunk.
    if semaphore.wait(timeout: .now()) == .success {
        writeQueue.async {
            stdout.write(bytes)
            semaphore.signal()
        }
    }
}

// ── 7. Start engine ───────────────────────────────────────────────────────────

do {
    try engine.start()
} catch {
    inputNode.removeTap(onBus: 0)
    AudioHardwareDestroyAggregateDevice(aggDeviceID)
    AudioHardwareDestroyProcessTap(tapID)
    fputs("error: engine start failed: \(error)\n", stderr)
    exit(1)
}

fputs("hush-audio-tap: streaming (sr=\(sampleRate) ch=\(channelCount))\n", stderr)

// ── 8. SIGTERM → clean shutdown ───────────────────────────────────────────────

let sigSource = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .main)
signal(SIGTERM, SIG_IGN)  // prevent default handler; DispatchSource handles it
sigSource.setEventHandler {
    inputNode.removeTap(onBus: 0)
    engine.stop()
    writeQueue.sync {}  // flush any pending stdout writes
    AudioHardwareDestroyAggregateDevice(aggDeviceID)
    AudioHardwareDestroyProcessTap(tapID)
    exit(0)
}
sigSource.resume()

RunLoop.main.run()
