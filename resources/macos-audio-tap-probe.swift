/// Minimal CoreAudio process-tap probe.
///
/// Purpose: confirm what macOS permission dialog appears when
/// `AudioHardwareCreateProcessTap` is invoked — specifically whether it
/// uses the Screen Recording TCC bucket or the `NSAudioCaptureUsageDescription`
/// bucket (audio-only consent, custom text, "Allow"/"Don't Allow" buttons).
///
/// See GitHub issue #585 and learnings.md entry 2026-05-06.
///
/// Build & run (see scripts/test-audio-tap-permission.sh):
///   swiftc resources/macos-audio-tap-probe.swift \
///       -framework CoreAudio -framework Foundation -framework AudioToolbox \
///       -o /tmp/hush-audio-tap-probe
///   codesign -s - /tmp/hush-audio-tap-probe
///   /tmp/hush-audio-tap-probe
///
/// Expected outcomes:
///   EXIT 0  "tap_created" — permission granted; tap created successfully.
///           The dialog that appeared is what we'll get in production.
///   EXIT 1  "permission_denied" — user clicked Don't Allow (or TCC denied).
///   EXIT 2  "unsupported" — macOS < 14.2, API not available.
///   EXIT 3  "error:<OSStatus>" — some other HAL error.

import AudioToolbox
import CoreAudio
import Foundation

guard #available(macOS 14.2, *) else {
    fputs("unsupported: macOS 14.2+ required\n", stderr)
    exit(2)
}

// Request a system-wide tap (processes = [] means all processes).
// This is the call that triggers the macOS permission dialog.
let desc = CATapDescription()
desc.name = "hush-probe"
desc.uuid = UUID()
desc.processes = []    // capture all system audio
desc.isMono = true
desc.isExclusive = false
desc.isMixdown = true
desc.isPrivate = true
desc.muteBehavior = .unmuted

var tapID = AudioObjectID(kAudioObjectUnknown)
let status = AudioHardwareCreateProcessTap(desc, &tapID)

switch status {
case noErr:
    // Success — note what the permission dialog looked like and clean up.
    fputs("tap_created: status=\(status) tapID=\(tapID)\n", stderr)
    AudioHardwareDestroyProcessTap(tapID)
    exit(0)

case kAudioHardwareIllegalOperationError:
    // -1 (illegal operation) is the TCC-denied error from AudioHardwareCreateProcessTap.
    fputs("permission_denied: OSStatus=\(status)\n", stderr)
    exit(1)

default:
    fputs("error: OSStatus=\(status)\n", stderr)
    exit(3)
}
