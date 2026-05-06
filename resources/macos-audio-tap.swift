/// CoreAudio process-tap audio streaming binary for Hush meeting mode.
///
/// Captures all system audio using `AudioHardwareCreateProcessTap` (macOS 14.2+)
/// and streams it as raw f32 LE interleaved PCM to stdout. Requires no Screen
/// Recording TCC permission — only the standard microphone-style audio consent
/// dialog (no pixels captured).
///
/// ## Key design decisions (see learnings.md #593)
///
/// - `isExclusive = true`: required for `processes = []` to mean "capture
///   everything." With `false`, the empty array means "tap no processes."
/// - `AudioDeviceCreateIOProcIDWithBlock` instead of AVAudioEngine: every
///   working open-source implementation (OpenWhispr, Korus, Atoll, yogurt)
///   uses a direct IOProc. AVAudioEngine's AUHAL resolves the aggregate's
///   main sub-device (output-only) and returns silence from its non-existent
///   input channels.
/// - Format is queried from the aggregate device input stream after the device
///   becomes alive — before starting the IOProc — so the HUSH header is always
///   written before any PCM arrives at the Rust reader.
/// - stdout I/O is offloaded to a writer queue (semaphore-bounded) so the
///   real-time IOProc callback is never blocked by a slow Rust reader.
///
/// Protocol:
///   stdout: 12-byte header (on successful start) followed by continuous PCM.
///   Header layout (little-endian):
///     bytes 0–3:  b"HUSH" magic
///     bytes 4–7:  sample_rate as u32
///     bytes 8–11: channel_count as u32
///
///   stderr: human-readable diagnostic messages.
///   Termination: SIGTERM causes clean shutdown (IOProc stopped, tap +
///   aggregate device destroyed).
///
/// Build (see src-tauri/build.rs::bundle_audio_tap_capture):
///   swiftc resources/macos-audio-tap.swift \
///       -framework CoreAudio -framework AudioToolbox \
///       -framework Foundation \
///       -o src-tauri/resources/hush-audio-tap-capture

import AudioToolbox
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
//
// `isExclusive = true` with `processes = []` means "exclude no one from the
// tap" — i.e. capture the whole system mix. With `false` the empty array
// delivers silence. Confirmed by OpenWhispr, Korus, Atoll, and yogurt source
// code — every working open-source implementation uses `true`. See learnings.md
// entry for #593.

let tapUUID = UUID()
let desc = CATapDescription()
desc.name = "hush-capture"
desc.uuid = tapUUID
desc.processes = []       // capture all system audio
desc.isMono = false       // stereo (downmixed by Whisper pipeline)
desc.isExclusive = true   // required: empty processes + exclusive = capture all
desc.isMixdown = true     // mix all process audio into one stream
desc.isPrivate = true     // don't expose tap as a public device
desc.muteBehavior = .unmuted

var tapID = AudioObjectID(kAudioObjectUnknown)
let tapStatus = AudioHardwareCreateProcessTap(desc, &tapID)
guard tapStatus == noErr else {
    fputs("error: tap creation failed: OSStatus=\(tapStatus)\n", stderr)
    exit(1)
}

// ── 3. Aggregate device with the tap ─────────────────────────────────────────
//
// Including the default output device in both SubDeviceList and as
// MainSubDevice ties the aggregate's clock to the system output clock —
// important for timestamp accuracy when mixing with microphone captures.
// TapAutoStart=false because we call AudioDeviceStart explicitly after
// writing the wire-protocol header.

let tapUID = tapUUID.uuidString
let aggUID = "io.github.khawkins98.hush.capture-\(UUID().uuidString)"
let outputUID = defaultOutputDeviceUID()

var subDeviceList: [[String: Any]] = []
if let uid = outputUID {
    subDeviceList = [[kAudioSubDeviceUIDKey as String: uid]]
}

var aggDesc: [String: Any] = [
    kAudioAggregateDeviceNameKey as String:      "HushCapture",
    kAudioAggregateDeviceUIDKey as String:       aggUID,
    kAudioAggregateDeviceIsPrivateKey as String: 1,
    kAudioAggregateDeviceIsStackedKey as String: 0,
    // TapAutoStart=false: we start the device ourselves after writing the
    // HUSH header so the Rust reader always sees the header before any PCM.
    kAudioAggregateDeviceTapAutoStartKey as String: false,
    kAudioAggregateDeviceSubDeviceListKey as String: subDeviceList,
    kAudioAggregateDeviceTapListKey as String: [
        // Drift compensation inserts/deletes samples to align the tap's clock
        // with the aggregate. PCM is no longer bit-identical to the source, but
        // for speech-to-text that's irrelevant and clock alignment is worth it.
        [kAudioSubTapUIDKey as String: tapUID,
         kAudioSubTapDriftCompensationKey as String: NSNumber(value: true)]
    ],
]
if let uid = outputUID {
    aggDesc[kAudioAggregateDeviceMainSubDeviceKey as String] = uid
}

var aggDeviceID = AudioObjectID(kAudioObjectUnknown)
let aggStatus = AudioHardwareCreateAggregateDevice(aggDesc as CFDictionary, &aggDeviceID)
guard aggStatus == noErr else {
    AudioHardwareDestroyProcessTap(tapID)
    fputs("error: aggregate device creation failed: OSStatus=\(aggStatus)\n", stderr)
    exit(1)
}

// ── 4. Wait for aggregate device to become alive ──────────────────────────────
//
// HAL registration is asynchronous; querying the format or starting the
// IOProc before the device is alive can fail or return a degenerate format.

var aliveAddr = AudioObjectPropertyAddress(
    mSelector: kAudioDevicePropertyDeviceIsAlive,
    mScope: kAudioObjectPropertyScopeGlobal,
    mElement: kAudioObjectPropertyElementMain)
var isAlive: UInt32 = 0
var aliveSize = UInt32(MemoryLayout<UInt32>.size)
for _ in 0..<20 {  // up to 200 ms
    let st = AudioObjectGetPropertyData(aggDeviceID, &aliveAddr, 0, nil, &aliveSize, &isAlive)
    if st != noErr {
        fputs("warning: alive-poll failed: OSStatus=\(st)\n", stderr)
        break
    }
    if isAlive != 0 { break }
    usleep(10_000)
}
if isAlive == 0 {
    fputs("warning: aggregate device not alive after 200 ms; continuing anyway\n", stderr)
}

// ── 5. Query format from aggregate device input stream ────────────────────────
//
// The input scope of the aggregate device exposes the tap's stream. Querying
// here (after the device is alive, before starting) gives us the sample rate
// and channel count we need for the HUSH wire-protocol header.
// Element 0 (main) is correct for the one-tap-one-input shape; if a second
// tap is ever added the aggregate gains multiple input streams and a stream
// enumeration via kAudioDevicePropertyStreams would be needed instead.

var streamFormatAddr = AudioObjectPropertyAddress(
    mSelector: kAudioDevicePropertyStreamFormat,
    mScope: kAudioObjectPropertyScopeInput,
    mElement: kAudioObjectPropertyElementMain)
var asbd = AudioStreamBasicDescription()
var asbdSize = UInt32(MemoryLayout<AudioStreamBasicDescription>.size)
let fmtStatus = AudioObjectGetPropertyData(
    aggDeviceID, &streamFormatAddr, 0, nil, &asbdSize, &asbd)

guard fmtStatus == noErr, asbd.mSampleRate > 0, asbd.mChannelsPerFrame > 0 else {
    AudioHardwareDestroyAggregateDevice(aggDeviceID)
    AudioHardwareDestroyProcessTap(tapID)
    fputs("error: failed to query stream format: OSStatus=\(fmtStatus) sr=\(asbd.mSampleRate) ch=\(asbd.mChannelsPerFrame)\n", stderr)
    exit(1)
}

let sampleRate = UInt32(asbd.mSampleRate)
let channelCount = asbd.mChannelsPerFrame
// CoreAudio process taps deliver non-interleaved f32 (one buffer per channel).
// We interleave in the IOProc before writing to stdout.
let isNonInterleaved = (asbd.mFormatFlags & kAudioFormatFlagIsNonInterleaved) != 0

// ── 5b. Scratch buffer pool ───────────────────────────────────────────────────
//
// Apple guidance: never call malloc/free inside an IOProc (malloc holds an
// internal lock → priority inversion risk on the audio thread).
// Solution: pre-allocate 32 raw slots sized to the device's nominal buffer,
// reuse them across callbacks. Pool count == semaphore bound (32) so slot N is
// never overwritten while its corresponding write is still in-flight.

var nominalFrames: UInt32 = 4096
var nominalFramesSize = UInt32(MemoryLayout<UInt32>.size)
var nominalFramesAddr = AudioObjectPropertyAddress(
    mSelector: kAudioDevicePropertyBufferFrameSize,
    mScope: kAudioObjectPropertyScopeInput,
    mElement: kAudioObjectPropertyElementMain)
// Ignore error: 4096 frames is a safe over-allocation if the query fails.
AudioObjectGetPropertyData(aggDeviceID, &nominalFramesAddr, 0, nil, &nominalFramesSize, &nominalFrames)

let poolCount = 32
let slotStride = Int(nominalFrames) * Int(channelCount) * MemoryLayout<Float>.size
let scratchPool: [UnsafeMutableRawPointer] = (0..<poolCount).map { _ in
    UnsafeMutableRawPointer.allocate(byteCount: slotStride, alignment: MemoryLayout<Float>.alignment)
}
// Only incremented by the audio thread (IOProc callbacks are serialised), so
// no atomic is needed.
var nextSlot = 0

// ── 6. SIGPIPE guard + write queue ───────────────────────────────────────────
//
// Ignore SIGPIPE before any stdout writes so a force-killed Rust parent
// doesn't terminate the helper before the DispatchSource cleanup runs.
// The write queue decouples stdout I/O from the real-time IOProc callback;
// a bounded semaphore (32 slots) drops chunks rather than blocking the
// audio thread when the Rust reader is slow.

signal(SIGPIPE, SIG_IGN)
let stdout = FileHandle.standardOutput
let writeQueue = DispatchQueue(label: "hush.audio.writer", qos: .userInteractive)
let semaphore = DispatchSemaphore(value: 32)
var droppedChunks: Int = 0

// ── 7. Write protocol header ──────────────────────────────────────────────────
//
// Written BEFORE AudioDeviceStart so the Rust reader always sees the header
// before any PCM arrives (wire-protocol invariant).

var header = Data(count: 12)
header.withUnsafeMutableBytes { raw in
    let p = raw.baseAddress!.assumingMemoryBound(to: UInt8.self)
    p[0] = 0x48; p[1] = 0x55; p[2] = 0x53; p[3] = 0x48  // "HUSH"
    var sr = sampleRate.littleEndian
    withUnsafeBytes(of: &sr) { bytes in for i in 0..<4 { p[4 + i] = bytes[i] } }
    var ch = channelCount.littleEndian
    withUnsafeBytes(of: &ch) { bytes in for i in 0..<4 { p[8 + i] = bytes[i] } }
}
stdout.write(header)

// ── 8. IOProc callback ────────────────────────────────────────────────────────
//
// AudioDeviceCreateIOProcIDWithBlock delivers inInputData from the aggregate
// device's input bus — which is the tap's PCM — regardless of whether the
// main sub-device has a physical microphone. This is why every working
// open-source implementation uses IOProc rather than AVAudioEngine.

var ioProcID: AudioDeviceIOProcID? = nil
let ioProcStatus = AudioDeviceCreateIOProcIDWithBlock(
    &ioProcID, aggDeviceID, nil
) { _, inInputData, _, _, _ in
    let buffers = UnsafeMutableAudioBufferListPointer(
        UnsafeMutablePointer(mutating: inInputData))
    guard !buffers.isEmpty,
          let first = buffers.first,
          first.mData != nil,
          first.mDataByteSize > 0 else { return }

    let chanCount = Int(channelCount)
    // For non-interleaved, each per-channel AudioBuffer has the same frame
    // count (all channels emitted in lockstep by an aggregate-device tap).
    let framesPerBuffer = Int(first.mDataByteSize) / MemoryLayout<Float>.size /
        (isNonInterleaved ? 1 : chanCount)
    guard framesPerBuffer > 0 else { return }

    let byteCount = framesPerBuffer * chanCount * MemoryLayout<Float>.size
    // Safety: if the device delivers more frames than the pool slot was sized
    // for (e.g. after a reconfiguration), skip rather than overflow.
    guard byteCount <= slotStride else { return }

    if semaphore.wait(timeout: .now()) == .success {
        // Pick the next pool slot. The semaphore bound == poolCount guarantees
        // this slot's previous write has already completed before we reuse it.
        let slot = nextSlot % poolCount
        nextSlot &+= 1

        // Interleave: non-interleaved has one AudioBuffer per channel; interleaved
        // has one buffer with all channels packed. Either way we produce wire-
        // format: interleaved f32 LE.
        let dst = scratchPool[slot].assumingMemoryBound(to: Float.self)
        if isNonInterleaved {
            for (ch, buf) in buffers.enumerated() where ch < chanCount {
                guard let data = buf.mData else { continue }
                let src = data.assumingMemoryBound(to: Float.self)
                for frame in 0..<framesPerBuffer {
                    dst[frame * chanCount + ch] = src[frame]
                }
            }
        } else {
            guard let data = buffers[0].mData else { semaphore.signal(); return }
            let src = data.assumingMemoryBound(to: Float.self)
            for i in 0..<(framesPerBuffer * chanCount) { dst[i] = src[i] }
        }

        // bytesNoCopy: no malloc — the slot stays live until signal() fires.
        let ptr = scratchPool[slot]
        let count = byteCount
        writeQueue.async {
            stdout.write(Data(bytesNoCopy: ptr, count: count, deallocator: .none))
            semaphore.signal()
        }
    } else {
        droppedChunks += 1
        let dropped = droppedChunks
        if dropped % 256 == 0 {
            writeQueue.async {
                fputs("hush-audio-tap: \(dropped) total chunks dropped (reader stalled)\n", stderr)
            }
        }
    }
}

guard ioProcStatus == noErr, ioProcID != nil else {
    AudioHardwareDestroyAggregateDevice(aggDeviceID)
    AudioHardwareDestroyProcessTap(tapID)
    fputs("error: IOProc creation failed: OSStatus=\(ioProcStatus)\n", stderr)
    exit(1)
}

// ── 9. Start device ───────────────────────────────────────────────────────────

let startStatus = AudioDeviceStart(aggDeviceID, ioProcID)
guard startStatus == noErr else {
    AudioDeviceDestroyIOProcID(aggDeviceID, ioProcID!)
    AudioHardwareDestroyAggregateDevice(aggDeviceID)
    AudioHardwareDestroyProcessTap(tapID)
    fputs("error: AudioDeviceStart failed: OSStatus=\(startStatus)\n", stderr)
    exit(1)
}

fputs("hush-audio-tap: streaming (sr=\(sampleRate) ch=\(channelCount))\n", stderr)

// ── 10. Signal handlers → clean shutdown ─────────────────────────────────────

func cleanup() {
    // Guard both calls: AudioDeviceStop(_, nil) has "stop default I/O" semantics
    // that would do the wrong thing if ioProcID were ever nil here.
    if let proc = ioProcID {
        AudioDeviceStop(aggDeviceID, proc)
        AudioDeviceDestroyIOProcID(aggDeviceID, proc)
    }
    writeQueue.sync {}  // flush any pending stdout writes
    for ptr in scratchPool { ptr.deallocate() }
    AudioHardwareDestroyAggregateDevice(aggDeviceID)
    AudioHardwareDestroyProcessTap(tapID)
    exit(0)
}

let sigTermSource = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .main)
signal(SIGTERM, SIG_IGN)  // prevent default handler; DispatchSource handles it
sigTermSource.setEventHandler { cleanup() }
sigTermSource.resume()

// SIGPIPE fires when the Rust parent is force-killed and the next write to
// stdout fails.  signal(SIGPIPE, SIG_IGN) above prevents the default handler;
// this source runs the explicit cleanup so the tap + aggregate are destroyed.
let sigPipeSource = DispatchSource.makeSignalSource(signal: SIGPIPE, queue: .main)
sigPipeSource.setEventHandler { cleanup() }
sigPipeSource.resume()

RunLoop.main.run()
