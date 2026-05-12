//! Event-driven microphone-activity monitor for meeting auto-start (#665).
//!
//! Replaces the 3-second foreground-app polling loop with a CoreAudio HAL
//! property listener on [`kAudioDevicePropertyDeviceIsRunningSomewhere`].
//! When any input device transitions between running and idle, the callback
//! fires and wakes the async meeting-detection task via a
//! [`tokio::sync::Notify`].
//!
//! # Memory-safety contract
//!
//! CoreAudio callbacks fire on an internal HAL thread. The raw `*mut c_void`
//! client-data pointer we pass is derived from [`Arc::as_ptr`] on a
//! [`Notify`] allocation that is kept alive by:
//!
//! 1. Every [`DeviceListenerHandle`] stores an `Arc<Notify>` clone.
//! 2. [`MicCameraMonitor`] stores its own `Arc<Notify>` clone.
//!
//! [`Drop for DeviceListenerHandle`] calls
//! `AudioObjectRemovePropertyListener` first. That function is
//! **synchronous**: it returns only after all in-flight callbacks have
//! completed. The `Arc` clone drops *after* the remove call, so the HAL
//! can never dereference a freed `Notify` pointer.
//!
//! # Platform
//!
//! macOS only — compiled under `#[cfg(target_os = "macos")]` in
//! `meeting/mod.rs`.

use std::ptr;
use std::sync::Arc;

use coreaudio_sys::{
    kAudioDevicePropertyDeviceIsRunningSomewhere, kAudioDevicePropertyStreamConfiguration,
    kAudioHardwarePropertyDevices, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeInput, kAudioObjectSystemObject,
    AudioObjectAddPropertyListener, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize,
    AudioObjectID, AudioObjectPropertyAddress, AudioObjectRemovePropertyListener, OSStatus,
};
use tokio::sync::Notify;

use super::MeetingAppKind;
use crate::meeting::MeetingAutostartMode;

// ── CoreAudio callback ────────────────────────────────────────────────────────

/// Invoked on a CoreAudio HAL thread when a registered property changes.
///
/// # Safety
///
/// `client_data` must be a pointer obtained from [`Arc::as_ptr`] on an
/// `Arc<Notify>` that is still alive (upheld by [`DeviceListenerHandle`]'s
/// drop ordering and [`MicCameraMonitor`]'s own Arc clone).
unsafe extern "C" fn on_property_changed(
    _object_id: AudioObjectID,
    _n_addresses: u32,
    _addresses: *const AudioObjectPropertyAddress,
    client_data: *mut ::std::os::raw::c_void,
) -> OSStatus {
    // SAFETY: client_data is Arc::as_ptr(&notify) — the Notify is alive
    // because the calling listener handle's Arc clone is still valid (the
    // Remove call in Drop is what ensures the callback cannot fire after
    // the Arc drops; see module-level safety contract).
    let notify = &*(client_data as *const Notify);
    notify.notify_one();
    0 // noErr
}

// ── Listener handles ─────────────────────────────────────────────────────────

/// RAII handle for a `kAudioDevicePropertyDeviceIsRunningSomewhere` listener
/// on a single audio device. Unregisters the listener on drop.
struct DeviceListenerHandle {
    device_id: AudioObjectID,
    /// Arc clone that keeps the `Notify` alive for as long as this listener
    /// is registered. Dropped *after* `AudioObjectRemovePropertyListener`
    /// returns in [`Drop`].
    notify: Arc<Notify>,
}

impl DeviceListenerHandle {
    fn new(device_id: AudioObjectID, notify: Arc<Notify>) -> Option<Self> {
        let address = running_somewhere_addr();
        let status = unsafe {
            AudioObjectAddPropertyListener(
                device_id,
                &address,
                Some(on_property_changed),
                Arc::as_ptr(&notify) as *mut _,
            )
        };
        if status != 0 {
            tracing::warn!(
                device_id,
                status,
                "AudioObjectAddPropertyListener failed for input device"
            );
            return None;
        }
        Some(Self { device_id, notify })
    }
}

impl Drop for DeviceListenerHandle {
    fn drop(&mut self) {
        let address = running_somewhere_addr();
        unsafe {
            // Synchronous: waits for any in-flight callback to finish before
            // returning. The Arc<Notify> drop on the next line is therefore
            // always safe — the HAL holds no live reference after this call.
            AudioObjectRemovePropertyListener(
                self.device_id,
                &address,
                Some(on_property_changed),
                Arc::as_ptr(&self.notify) as *mut _,
            );
        }
        // Arc<Notify> drops here.
    }
}

/// RAII handle for the system-object `kAudioHardwarePropertyDevices` listener
/// (fires when devices are added or removed — hot-plug / unplug).
struct SystemListenerHandle {
    notify: Arc<Notify>,
}

impl SystemListenerHandle {
    fn new(notify: Arc<Notify>) -> Option<Self> {
        let address = devices_list_addr();
        let status = unsafe {
            AudioObjectAddPropertyListener(
                kAudioObjectSystemObject as AudioObjectID,
                &address,
                Some(on_property_changed),
                Arc::as_ptr(&notify) as *mut _,
            )
        };
        if status != 0 {
            tracing::warn!(
                status,
                "AudioObjectAddPropertyListener failed for system-object device list"
            );
            return None;
        }
        Some(Self { notify })
    }
}

impl Drop for SystemListenerHandle {
    fn drop(&mut self) {
        let address = devices_list_addr();
        unsafe {
            AudioObjectRemovePropertyListener(
                kAudioObjectSystemObject as AudioObjectID,
                &address,
                Some(on_property_changed),
                Arc::as_ptr(&self.notify) as *mut _,
            );
        }
    }
}

// ── Public monitor ────────────────────────────────────────────────────────────

/// Monitors input audio-device activity via CoreAudio HAL property listeners.
///
/// Callers await [`wait_for_change`] and then call [`is_any_device_active`]
/// to get the current state. Call [`refresh_devices`] after a device-list
/// change (the same `Notify` fires for both hot-plug and device-state events,
/// so it is cheapest to refresh on every notification rather than trying to
/// discriminate the source).
pub struct MicCameraMonitor {
    /// Shared with all listener handles. Kept alive here so the Notify
    /// outlives every DeviceListenerHandle even if they are all cleared.
    notify: Arc<Notify>,
    device_listeners: Vec<DeviceListenerHandle>,
    _system_listener: Option<SystemListenerHandle>,
}

impl Default for MicCameraMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl MicCameraMonitor {
    /// Enumerate input devices, install HAL listeners, and install the
    /// hot-plug listener on the system object.
    pub fn new() -> Self {
        let notify = Arc::new(Notify::new());
        let device_listeners =
            install_device_listeners(get_input_device_ids(), Arc::clone(&notify));
        let system_listener = SystemListenerHandle::new(Arc::clone(&notify));
        Self {
            notify,
            device_listeners,
            _system_listener: system_listener,
        }
    }

    /// Returns `true` if any tracked input device has
    /// `kAudioDevicePropertyDeviceIsRunningSomewhere == 1` right now.
    pub fn is_any_device_active(&self) -> bool {
        self.device_listeners
            .iter()
            .any(|h| is_device_active(h.device_id))
    }

    /// Resolves the next time any registered listener fires (device-state
    /// change or hot-plug). This is edge-triggered: each `notify_one()` from
    /// the HAL thread is consumed exactly once by the next `notified()` call.
    pub async fn wait_for_change(&self) {
        self.notify.notified().await;
    }

    /// Re-enumerate input devices and update listener registrations. Call
    /// after receiving a notification to handle hot-plug / unplug events.
    /// Stale handles are dropped (which unregisters their listeners) before
    /// the new set is installed.
    pub fn refresh_devices(&mut self) {
        self.device_listeners.clear();
        self.device_listeners =
            install_device_listeners(get_input_device_ids(), Arc::clone(&self.notify));
    }
}

// ── CoreAudio helpers ─────────────────────────────────────────────────────────

fn running_somewhere_addr() -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyDeviceIsRunningSomewhere,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

fn devices_list_addr() -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyDevices,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// Returns all audio device IDs known to the system object.
fn get_all_device_ids() -> Vec<AudioObjectID> {
    let address = devices_list_addr();
    let mut data_size: u32 = 0;
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            kAudioObjectSystemObject as AudioObjectID,
            &address,
            0,
            ptr::null(),
            &mut data_size,
        )
    };
    if status != 0 || data_size == 0 {
        return Vec::new();
    }
    let count = data_size as usize / std::mem::size_of::<AudioObjectID>();
    let mut ids = vec![0u32; count];
    let mut actual = data_size;
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as AudioObjectID,
            &address,
            0,
            ptr::null(),
            &mut actual,
            ids.as_mut_ptr() as *mut _,
        )
    };
    if status != 0 {
        Vec::new()
    } else {
        ids
    }
}

/// Filters [`get_all_device_ids`] to only devices with at least one input
/// stream (i.e. excludes output-only devices like speakers).
fn get_input_device_ids() -> Vec<AudioObjectID> {
    get_all_device_ids()
        .into_iter()
        .filter(|&id| is_input_device(id))
        .collect()
}

/// Returns `true` if the device has at least one input stream buffer by
/// querying `kAudioDevicePropertyStreamConfiguration` with
/// `kAudioObjectPropertyScopeInput`. Excludes output-only devices (speakers,
/// headphone jacks) that would produce false positives for
/// `kAudioDevicePropertyDeviceIsRunningSomewhere` during music playback.
fn is_input_device(device_id: AudioObjectID) -> bool {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyStreamConfiguration,
        mScope: kAudioObjectPropertyScopeInput,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut data_size: u32 = 0;
    let status = unsafe {
        AudioObjectGetPropertyDataSize(device_id, &address, 0, ptr::null(), &mut data_size)
    };
    // data_size == 0 means no input-scope stream config → output-only device.
    if status != 0 || data_size < 4 {
        return false;
    }
    let mut buf = vec![0u8; data_size as usize];
    let mut actual = data_size;
    let status = unsafe {
        AudioObjectGetPropertyData(
            device_id,
            &address,
            0,
            ptr::null(),
            &mut actual,
            buf.as_mut_ptr() as *mut _,
        )
    };
    if status != 0 {
        return false;
    }
    // First 4 bytes of AudioBufferList are mNumberBuffers (UInt32, LE).
    let n_buffers = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
    n_buffers > 0
}

/// Query `kAudioDevicePropertyDeviceIsRunningSomewhere` for one device.
fn is_device_active(device_id: AudioObjectID) -> bool {
    let address = running_somewhere_addr();
    let mut running: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            device_id,
            &address,
            0,
            ptr::null(),
            &mut size,
            &mut running as *mut u32 as *mut _,
        )
    };
    status == 0 && running != 0
}

/// Register `kAudioDevicePropertyDeviceIsRunningSomewhere` listeners for the
/// given device IDs, all sharing the same `Notify`. Returns the handles for
/// successfully registered listeners.
fn install_device_listeners(
    device_ids: Vec<AudioObjectID>,
    notify: Arc<Notify>,
) -> Vec<DeviceListenerHandle> {
    device_ids
        .into_iter()
        .filter_map(|id| DeviceListenerHandle::new(id, Arc::clone(&notify)))
        .collect()
}

// ── State-machine logic (pure, unit-testable) ────────────────────────────────

/// All inputs to one evaluation of the mic auto-start state machine.
#[derive(Debug, Clone)]
pub struct MicStateInputs {
    /// Is any tracked input device currently running?
    pub mic_is_active: bool,
    /// User's configured auto-start mode.
    pub mode: MeetingAutostartMode,
    /// Is a meeting session already open in the manager?
    pub session_active: bool,
    /// Did we already emit a session start during this mic-activation cycle?
    /// Reset to `false` when `mic_is_active` flips back to `false`.
    pub session_emitted: bool,
    /// Classification of the frontmost app at evaluation time.
    pub frontmost_app_kind: MeetingAppKind,
    /// Name (bundle ID or display name) of the frontmost app.
    pub frontmost_app_name: String,
}

/// Outcome of one evaluation of the auto-start state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MicStateOutcome {
    /// No action needed; keep `session_emitted` as-is.
    Idle,
    /// Mic went quiet — caller must reset `session_emitted = false`.
    ResetSessionEmitted,
    /// Start a new meeting session for the named app. Caller must set
    /// `session_emitted = true` and call `start_manual`.
    Start { app_name: String },
}

/// Pure decision function for the mic auto-start state machine.
///
/// Called on each property-change event (and once at startup). All inputs
/// are explicit so the function is fully unit-testable without CoreAudio.
pub fn evaluate_mic_state(inputs: &MicStateInputs) -> MicStateOutcome {
    if !inputs.mic_is_active {
        return MicStateOutcome::ResetSessionEmitted;
    }
    // Mic is active from here.
    if inputs.mode == MeetingAutostartMode::Off {
        return MicStateOutcome::Idle;
    }
    if inputs.session_active {
        return MicStateOutcome::Idle;
    }
    if inputs.session_emitted {
        return MicStateOutcome::Idle;
    }
    if inputs.frontmost_app_kind != MeetingAppKind::Meeting {
        return MicStateOutcome::Idle;
    }
    MicStateOutcome::Start {
        app_name: inputs.frontmost_app_name.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(
        mic_active: bool,
        mode: MeetingAutostartMode,
        session_active: bool,
        session_emitted: bool,
        app_kind: MeetingAppKind,
    ) -> MicStateInputs {
        MicStateInputs {
            mic_is_active: mic_active,
            mode,
            session_active,
            session_emitted,
            frontmost_app_kind: app_kind,
            frontmost_app_name: "us.zoom.xos".into(),
        }
    }

    #[test]
    fn mic_inactive_always_resets_session_emitted() {
        let i = inputs(
            false,
            MeetingAutostartMode::Always,
            false,
            true,
            MeetingAppKind::Meeting,
        );
        assert_eq!(evaluate_mic_state(&i), MicStateOutcome::ResetSessionEmitted);
    }

    #[test]
    fn mic_inactive_resets_even_in_off_mode() {
        // Off mode is checked after the mic-active guard, so a quiet mic
        // still resets the session_emitted flag so the next activation
        // cycle can potentially start a session (if mode changes to Always).
        let i = inputs(
            false,
            MeetingAutostartMode::Off,
            false,
            true,
            MeetingAppKind::Meeting,
        );
        assert_eq!(evaluate_mic_state(&i), MicStateOutcome::ResetSessionEmitted);
    }

    #[test]
    fn off_mode_is_idle_when_mic_active() {
        let i = inputs(
            true,
            MeetingAutostartMode::Off,
            false,
            false,
            MeetingAppKind::Meeting,
        );
        assert_eq!(evaluate_mic_state(&i), MicStateOutcome::Idle);
    }

    #[test]
    fn session_already_active_is_idle() {
        let i = inputs(
            true,
            MeetingAutostartMode::Always,
            true,
            false,
            MeetingAppKind::Meeting,
        );
        assert_eq!(evaluate_mic_state(&i), MicStateOutcome::Idle);
    }

    #[test]
    fn session_already_emitted_is_idle() {
        let i = inputs(
            true,
            MeetingAutostartMode::Always,
            false,
            true,
            MeetingAppKind::Meeting,
        );
        assert_eq!(evaluate_mic_state(&i), MicStateOutcome::Idle);
    }

    #[test]
    fn non_meeting_app_is_idle() {
        for kind in [MeetingAppKind::Other, MeetingAppKind::Media] {
            let i = inputs(true, MeetingAutostartMode::Always, false, false, kind);
            assert_eq!(
                evaluate_mic_state(&i),
                MicStateOutcome::Idle,
                "expected Idle for {kind:?}"
            );
        }
    }

    #[test]
    fn meeting_app_with_active_mic_triggers_start() {
        let i = inputs(
            true,
            MeetingAutostartMode::Always,
            false,
            false,
            MeetingAppKind::Meeting,
        );
        assert_eq!(
            evaluate_mic_state(&i),
            MicStateOutcome::Start {
                app_name: "us.zoom.xos".into()
            }
        );
    }

    #[test]
    fn start_carries_correct_app_name() {
        let mut i = inputs(
            true,
            MeetingAutostartMode::Always,
            false,
            false,
            MeetingAppKind::Meeting,
        );
        i.frontmost_app_name = "com.microsoft.teams2".into();
        match evaluate_mic_state(&i) {
            MicStateOutcome::Start { app_name } => {
                assert_eq!(app_name, "com.microsoft.teams2");
            }
            other => panic!("expected Start, got {other:?}"),
        }
    }
}
