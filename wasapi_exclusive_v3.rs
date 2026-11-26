#![allow(non_snake_case)]

#[cfg(not(windows))]
compile_error!("wasapi_exclusive_v3 is only supported on Windows");

use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

type HResult = i32;

const ERR_OK: i32 = 0;
const ERR_BAD_HANDLE: i32 = -1;
const ERR_BAD_ARGS: i32 = -2;
const ERR_COINIT: i32 = -3;
const ERR_NO_DEVICE: i32 = -4;
const ERR_ACTIVATE: i32 = -5;
const ERR_INIT: i32 = -6;
const ERR_FORMAT: i32 = -7;
const ERR_GETSERVICE: i32 = -8;
const ERR_STARTRUN: i32 = -9;
const ERR_WRITE: i32 = -10;

const S_OK: HResult = 0;
const S_FALSE: HResult = 1;
const RPC_E_CHANGED_MODE: HResult = 0x8001_0106u32 as i32;
const AUDCLNT_E_UNSUPPORTED_FORMAT: HResult = 0x8889_0008u32 as i32;
const COINIT_MULTITHREADED: u32 = 0;
const CLSCTX_ALL: u32 = 23;
const AUDCLNT_SHAREMODE_EXCLUSIVE: i32 = 0;
const WAVE_FORMAT_PCM: u16 = 1;
const BEEP_SAMPLE_RATE: i32 = 48_000;
const BEEP_CHANNELS: usize = 2;

#[repr(C)]
#[derive(Clone, Copy)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

const CLSID_MMDEVICE_ENUMERATOR: Guid = Guid {
    data1: 0xBCDE_0395,
    data2: 0xE52F,
    data3: 0x467C,
    data4: [0x8E, 0x3D, 0xC4, 0x57, 0x92, 0x91, 0x69, 0x2E],
};
const IID_IMMDEVICE_ENUMERATOR: Guid = Guid {
    data1: 0xA956_64D2,
    data2: 0x9614,
    data3: 0x4F35,
    data4: [0xA7, 0x46, 0xDE, 0x8D, 0xB6, 0x36, 0x17, 0xE6],
};
const IID_IAUDIO_CLIENT: Guid = Guid {
    data1: 0x1CB9_AD4C,
    data2: 0xDBFA,
    data3: 0x4C32,
    data4: [0xB1, 0x78, 0xC2, 0xF5, 0x68, 0xA7, 0x03, 0xB2],
};
const IID_IAUDIO_RENDER_CLIENT: Guid = Guid {
    data1: 0xF294_ACFC,
    data2: 0x3146,
    data3: 0x4483,
    data4: [0xA7, 0xBF, 0xAD, 0xDC, 0xA7, 0xC2, 0x60, 0xE2],
};

#[repr(C)]
struct WaveFormatEx {
    format_tag: u16,
    channels: u16,
    samples_per_sec: u32,
    avg_bytes_per_sec: u32,
    block_align: u16,
    bits_per_sample: u16,
    extra_size: u16,
}

#[repr(C)]
struct ImmDeviceEnumerator {
    vtable: *const ImmDeviceEnumeratorVTable,
}

#[repr(C)]
struct ImmDeviceEnumeratorVTable {
    query_interface: usize,
    add_ref: usize,
    release: unsafe extern "system" fn(*mut ImmDeviceEnumerator) -> u32,
    enum_audio_endpoints: usize,
    get_default_audio_endpoint: unsafe extern "system" fn(
        *mut ImmDeviceEnumerator,
        i32,
        i32,
        *mut *mut ImmDevice,
    ) -> HResult,
}

#[repr(C)]
struct ImmDevice {
    vtable: *const ImmDeviceVTable,
}

#[repr(C)]
struct ImmDeviceVTable {
    query_interface: usize,
    add_ref: usize,
    release: unsafe extern "system" fn(*mut ImmDevice) -> u32,
    activate: unsafe extern "system" fn(
        *mut ImmDevice,
        *const Guid,
        u32,
        *mut c_void,
        *mut *mut c_void,
    ) -> HResult,
    open_property_store: usize,
    get_id: usize,
    get_state: usize,
}

#[repr(C)]
struct AudioClient {
    vtable: *const AudioClientVTable,
}

#[repr(C)]
struct AudioClientVTable {
    query_interface: usize,
    add_ref: usize,
    release: unsafe extern "system" fn(*mut AudioClient) -> u32,
    initialize: unsafe extern "system" fn(
        *mut AudioClient,
        i32,
        u32,
        i64,
        i64,
        *const WaveFormatEx,
        *const Guid,
    ) -> HResult,
    get_buffer_size: unsafe extern "system" fn(*mut AudioClient, *mut u32) -> HResult,
    get_stream_latency: usize,
    get_current_padding: unsafe extern "system" fn(*mut AudioClient, *mut u32) -> HResult,
    is_format_supported: usize,
    get_mix_format: usize,
    get_device_period: usize,
    start: unsafe extern "system" fn(*mut AudioClient) -> HResult,
    stop: unsafe extern "system" fn(*mut AudioClient) -> HResult,
    reset: usize,
    set_event_handle: usize,
    get_service: unsafe extern "system" fn(
        *mut AudioClient,
        *const Guid,
        *mut *mut c_void,
    ) -> HResult,
}

#[repr(C)]
struct AudioRenderClient {
    vtable: *const AudioRenderClientVTable,
}

#[repr(C)]
struct AudioRenderClientVTable {
    query_interface: usize,
    add_ref: usize,
    release: unsafe extern "system" fn(*mut AudioRenderClient) -> u32,
    get_buffer:
        unsafe extern "system" fn(*mut AudioRenderClient, u32, *mut *mut u8) -> HResult,
    release_buffer: unsafe extern "system" fn(*mut AudioRenderClient, u32, u32) -> HResult,
}

#[link(name = "ole32")]
extern "system" {
    fn CoInitializeEx(reserved: *mut c_void, co_init: u32) -> HResult;
    fn CoUninitialize();
    fn CoCreateInstance(
        clsid: *const Guid,
        outer: *mut c_void,
        context: u32,
        iid: *const Guid,
        object: *mut *mut c_void,
    ) -> HResult;
}

struct Session {
    client: *mut AudioClient,
    render: *mut AudioRenderClient,
    format: Box<WaveFormatEx>,
    frames_per_buffer: u32,
    frame_size: u32,
    started: bool,
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            if !self.render.is_null() {
                ((*(*self.render).vtable).release)(self.render);
                self.render = ptr::null_mut();
            }
            if !self.client.is_null() {
                ((*(*self.client).vtable).release)(self.client);
                self.client = ptr::null_mut();
            }
        }
    }
}

fn com_enter() -> Result<bool, i32> {
    let hr = unsafe { CoInitializeEx(ptr::null_mut(), COINIT_MULTITHREADED) };
    if hr == S_OK || hr == S_FALSE {
        Ok(true)
    } else if hr == RPC_E_CHANGED_MODE {
        Ok(false)
    } else {
        Err(ERR_COINIT)
    }
}

fn com_leave(need_uninitialize: bool) {
    if need_uninitialize {
        unsafe { CoUninitialize() };
    }
}

fn open_session(
    sample_rate: i32,
    channels: i32,
    bits_per_sample: i32,
    buffer_ms: i32,
) -> Result<Box<Session>, i32> {
    if sample_rate <= 0
        || channels <= 0
        || channels > u16::MAX as i32
        || (bits_per_sample != 16 && bits_per_sample != 24)
        || buffer_ms <= 0
    {
        return Err(ERR_BAD_ARGS);
    }

    let block_align = (channels as u32)
        .checked_mul((bits_per_sample / 8) as u32)
        .filter(|value| *value <= u16::MAX as u32)
        .ok_or(ERR_BAD_ARGS)?;
    let avg_bytes_per_sec = (sample_rate as u32)
        .checked_mul(block_align)
        .ok_or(ERR_BAD_ARGS)?;

    let format = Box::new(WaveFormatEx {
        format_tag: WAVE_FORMAT_PCM,
        channels: channels as u16,
        samples_per_sec: sample_rate as u32,
        avg_bytes_per_sec,
        block_align: block_align as u16,
        bits_per_sample: bits_per_sample as u16,
        extra_size: 0,
    });
    let mut session = Box::new(Session {
        client: ptr::null_mut(),
        render: ptr::null_mut(),
        format,
        frames_per_buffer: 0,
        frame_size: block_align,
        started: false,
    });

    let need_uninitialize = com_enter()?;
    let mut enumerator: *mut ImmDeviceEnumerator = ptr::null_mut();
    let mut device: *mut ImmDevice = ptr::null_mut();

    let result = unsafe {
        (|| -> Result<(), i32> {
            let hr = CoCreateInstance(
                &CLSID_MMDEVICE_ENUMERATOR,
                ptr::null_mut(),
                CLSCTX_ALL,
                &IID_IMMDEVICE_ENUMERATOR,
                &mut enumerator as *mut *mut ImmDeviceEnumerator as *mut *mut c_void,
            );
            if hr < 0 {
                return Err(ERR_NO_DEVICE);
            }

            let hr = ((*(*enumerator).vtable).get_default_audio_endpoint)(
                enumerator, 0, 0, &mut device,
            );
            if hr < 0 {
                return Err(ERR_NO_DEVICE);
            }

            let mut client: *mut c_void = ptr::null_mut();
            let hr = ((*(*device).vtable).activate)(
                device,
                &IID_IAUDIO_CLIENT,
                CLSCTX_ALL,
                ptr::null_mut(),
                &mut client,
            );
            if hr < 0 {
                return Err(ERR_ACTIVATE);
            }
            session.client = client as *mut AudioClient;

            let duration = (buffer_ms as i64) * 10_000;
            let hr = ((*(*session.client).vtable).initialize)(
                session.client,
                AUDCLNT_SHAREMODE_EXCLUSIVE,
                0,
                duration,
                duration,
                &*session.format,
                ptr::null(),
            );
            if hr < 0 {
                return Err(if hr == AUDCLNT_E_UNSUPPORTED_FORMAT {
                    ERR_FORMAT
                } else {
                    ERR_INIT
                });
            }

            let mut buffer_size = 0u32;
            let hr = ((*(*session.client).vtable).get_buffer_size)(
                session.client,
                &mut buffer_size,
            );
            if hr < 0 || buffer_size == 0 {
                return Err(ERR_INIT);
            }
            session.frames_per_buffer = buffer_size;

            let mut render: *mut c_void = ptr::null_mut();
            let hr = ((*(*session.client).vtable).get_service)(
                session.client,
                &IID_IAUDIO_RENDER_CLIENT,
                &mut render,
            );
            if hr < 0 {
                return Err(ERR_GETSERVICE);
            }
            session.render = render as *mut AudioRenderClient;

            let hr = ((*(*session.client).vtable).start)(session.client);
            if hr < 0 {
                return Err(ERR_STARTRUN);
            }
            session.started = true;
            Ok(())
        })()
    };

    unsafe {
        if !device.is_null() {
            ((*(*device).vtable).release)(device);
        }
        if !enumerator.is_null() {
            ((*(*enumerator).vtable).release)(enumerator);
        }
    }
    let outcome = match result {
        Ok(()) => Ok(session),
        Err(error) => {
            drop(session);
            Err(error)
        }
    };
    com_leave(need_uninitialize);
    outcome
}

fn write_frames(session: &Session, data: *const u8, data_len_frames: u32) -> i32 {
    let mut source = data;
    let mut remaining = data_len_frames;
    let capacity = session.frames_per_buffer;

    while remaining > 0 {
        let mut padding = 0u32;
        let hr = unsafe {
            ((*(*session.client).vtable).get_current_padding)(session.client, &mut padding)
        };
        if hr < 0 {
            return hr;
        }
        if padding >= capacity {
            thread::sleep(Duration::from_millis(1));
            continue;
        }

        let chunk = remaining.min(capacity - padding);
        let mut destination: *mut u8 = ptr::null_mut();
        let hr = unsafe {
            ((*(*session.render).vtable).get_buffer)(
                session.render,
                chunk,
                &mut destination,
            )
        };
        if hr < 0 {
            return hr;
        }

        let byte_count = (chunk as usize) * (session.frame_size as usize);
        unsafe {
            ptr::copy_nonoverlapping(source, destination, byte_count);
            source = source.add(byte_count);
        }
        remaining -= chunk;

        let hr = unsafe {
            ((*(*session.render).vtable).release_buffer)(session.render, chunk, 0)
        };
        if hr < 0 {
            return hr;
        }
    }
    ERR_OK
}

fn with_com_write(session: &Session, data: *const u8, frames: u32) -> i32 {
    let need_uninitialize = match com_enter() {
        Ok(value) => value,
        Err(error) => return error,
    };
    let result = write_frames(session, data, frames);
    com_leave(need_uninitialize);
    result
}

fn close_owned_session(mut session: Box<Session>) -> i32 {
    let need_uninitialize = match com_enter() {
        Ok(value) => value,
        Err(error) => {
            std::mem::forget(session);
            return error;
        }
    };
    if session.started {
        unsafe {
            ((*(*session.client).vtable).stop)(session.client);
        }
        session.started = false;
    }
    drop(session);
    com_leave(need_uninitialize);
    ERR_OK
}

#[no_mangle]
pub unsafe extern "C" fn wasapi_open(
    out_session: *mut *mut c_void,
    sample_rate: i32,
    channels: i32,
    bits_per_sample: i32,
    buffer_ms: i32,
) -> i32 {
    if out_session.is_null() {
        return ERR_BAD_ARGS;
    }
    *out_session = ptr::null_mut();

    match open_session(sample_rate, channels, bits_per_sample, buffer_ms) {
        Ok(session) => {
            *out_session = Box::into_raw(session) as *mut c_void;
            ERR_OK
        }
        Err(error) => error,
    }
}

#[no_mangle]
pub unsafe extern "C" fn wasapi_get_free_space(
    session_handle: *mut c_void,
    frames_free: *mut i32,
) -> i32 {
    if session_handle.is_null() || frames_free.is_null() {
        return ERR_BAD_HANDLE;
    }
    let session = &*(session_handle as *mut Session);
    let need_uninitialize = match com_enter() {
        Ok(value) => value,
        Err(error) => return error,
    };

    let mut padding = 0u32;
    let hr = ((*(*session.client).vtable).get_current_padding)(session.client, &mut padding);
    let result = if hr < 0 {
        hr
    } else {
        *frames_free = session
            .frames_per_buffer
            .saturating_sub(padding)
            .min(i32::MAX as u32) as i32;
        ERR_OK
    };
    com_leave(need_uninitialize);
    result
}

#[no_mangle]
pub unsafe extern "C" fn wasapi_write(
    session_handle: *mut c_void,
    data: *const c_void,
    data_len_frames: i32,
) -> i32 {
    if session_handle.is_null() || data.is_null() || data_len_frames < 0 {
        return ERR_BAD_HANDLE;
    }
    let session = &*(session_handle as *mut Session);
    let need_uninitialize = match com_enter() {
        Ok(value) => value,
        Err(error) => return error,
    };
    let result = write_frames(session, data as *const u8, data_len_frames as u32);
    com_leave(need_uninitialize);
    result
}

#[no_mangle]
pub unsafe extern "C" fn wasapi_close(session_handle: *mut c_void) -> i32 {
    if session_handle.is_null() {
        return ERR_BAD_HANDLE;
    }
    let need_uninitialize = match com_enter() {
        Ok(value) => value,
        Err(error) => return error,
    };
    let mut session = Box::from_raw(session_handle as *mut Session);
    if session.started {
        ((*(*session.client).vtable).stop)(session.client);
        session.started = false;
    }
    drop(session);
    com_leave(need_uninitialize);
    ERR_OK
}

#[derive(Clone, Copy)]
struct BeepState {
    initialized: bool,
    handle: usize,
    ready: bool,
    stop: bool,
    result: i32,
    mode: i32,
    frequency: i32,
    amplitude: f64,
    remaining_frames: i64,
}

struct BeepController {
    state: Mutex<BeepState>,
    changed: Condvar,
    worker: Mutex<Option<JoinHandle<()>>>,
}

fn beep_controller() -> &'static BeepController {
    static CONTROLLER: OnceLock<BeepController> = OnceLock::new();
    CONTROLLER.get_or_init(|| BeepController {
        state: Mutex::new(BeepState {
            initialized: false,
            handle: 0,
            ready: false,
            stop: false,
            result: ERR_OK,
            mode: 0,
            frequency: 0,
            amplitude: 0.0,
            remaining_frames: 0,
        }),
        changed: Condvar::new(),
        worker: Mutex::new(None),
    })
}

fn lock_beep_state() -> Result<MutexGuard<'static, BeepState>, i32> {
    beep_controller().state.lock().map_err(|_| ERR_INIT)
}

fn next_beep_handle() -> Result<usize, i32> {
    static NEXT_HANDLE: AtomicUsize = AtomicUsize::new(1);
    NEXT_HANDLE
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| ERR_INIT)
}

// Beep handles are opaque numeric tokens and are never dereferenced.
fn has_beep_handle(state: &BeepState, handle: *mut c_void) -> bool {
    !handle.is_null() && state.initialized && state.handle == handle as usize
}

fn beep_worker_main() -> i32 {
    let session = match open_session(BEEP_SAMPLE_RATE, BEEP_CHANNELS as i32, 16, 10) {
        Ok(session) => session,
        Err(error) => return error,
    };

    let sample_count = match (session.frames_per_buffer as usize).checked_mul(BEEP_CHANNELS) {
        Some(count) => count,
        None => return close_with_error(session, ERR_INIT),
    };
    let mut samples = Vec::<i16>::new();
    if samples.try_reserve_exact(sample_count).is_err() {
        return close_with_error(session, ERR_INIT);
    }
    samples.resize(sample_count, 0);

    match lock_beep_state() {
        Ok(mut state) => {
            state.ready = true;
            beep_controller().changed.notify_all();
        }
        Err(error) => return close_with_error(session, error),
    }

    let mut previous_frequency = 0;
    let mut previous_amplitude = 0.0f64;
    let mut previous_mode = 0;
    let mut ramp_frames = 0i32;
    let mut current_amplitude = 0.0f64;
    let mut phase = 0.0f64;
    let pi = std::f64::consts::PI;

    loop {
        let state = match lock_beep_state() {
            Ok(state) => *state,
            Err(error) => return close_with_error(session, error),
        };
        if state.stop {
            break;
        }

        let tone_frames = if state.mode == 1 {
            session.frames_per_buffer
        } else if state.mode == 2 {
            (state.remaining_frames.max(0) as u32).min(session.frames_per_buffer)
        } else {
            0
        };
        let final_chunk = state.mode == 2
            && state.remaining_frames <= session.frames_per_buffer as i64;

        if state.mode != previous_mode
            || state.frequency != previous_frequency
            || state.amplitude != previous_amplitude
        {
            previous_mode = state.mode;
            previous_frequency = state.frequency;
            previous_amplitude = state.amplitude;
            ramp_frames = BEEP_SAMPLE_RATE / 200;
        }

        for frame in 0..session.frames_per_buffer as usize {
            let mut frame_target = if frame < tone_frames as usize {
                state.amplitude
            } else {
                0.0
            };
            if final_chunk
                && frame < tone_frames as usize
                && tone_frames as usize - frame <= (BEEP_SAMPLE_RATE / 200) as usize
            {
                frame_target *=
                    (tone_frames as usize - frame) as f64 / (BEEP_SAMPLE_RATE / 200) as f64;
            }

            if ramp_frames > 0 {
                current_amplitude += (frame_target - current_amplitude) / ramp_frames as f64;
                ramp_frames -= 1;
            } else {
                current_amplitude = frame_target;
            }

            let value = (phase.sin() * 32767.0 * current_amplitude) as i16;
            samples[frame * BEEP_CHANNELS] = value;
            samples[frame * BEEP_CHANNELS + 1] = value;

            if frame < tone_frames as usize && state.frequency != 0 {
                phase += 2.0 * pi * state.frequency as f64 / BEEP_SAMPLE_RATE as f64;
                if phase >= 2.0 * pi {
                    phase -= 2.0 * pi;
                }
            }
        }

        let write_result = with_com_write(
            &session,
            samples.as_ptr() as *const u8,
            session.frames_per_buffer,
        );
        if write_result != ERR_OK {
            if let Ok(mut current) = lock_beep_state() {
                current.result = write_result;
                beep_controller().changed.notify_all();
            }
            return close_with_error(session, write_result);
        }

        if state.mode == 2 {
            if let Ok(mut current) = lock_beep_state() {
                if current.mode == 2 {
                    current.remaining_frames =
                        (current.remaining_frames - tone_frames as i64).max(0);
                    if current.remaining_frames == 0 {
                        current.mode = 0;
                        current.frequency = 0;
                        beep_controller().changed.notify_all();
                    }
                }
            }
        }
    }

    close_owned_session(session)
}

fn close_with_error(session: Box<Session>, original_error: i32) -> i32 {
    let close_result = close_owned_session(session);
    if close_result == ERR_OK {
        original_error
    } else {
        close_result
    }
}

fn beep_worker() {
    let result = beep_worker_main();
    if let Ok(mut state) = lock_beep_state() {
        if state.result == ERR_OK && result != ERR_OK {
            state.result = result;
        }
        state.ready = true;
        state.mode = 0;
        beep_controller().changed.notify_all();
    }
}

#[no_mangle]
pub unsafe extern "C" fn wasapi_beep_init(out_handle: *mut *mut c_void) -> i32 {
    if out_handle.is_null() {
        return ERR_BAD_ARGS;
    }
    *out_handle = ptr::null_mut();

    let handle = {
        let mut state = match lock_beep_state() {
            Ok(state) => state,
            Err(error) => return error,
        };
        if state.initialized {
            return ERR_BAD_ARGS;
        }
        let handle = match next_beep_handle() {
            Ok(handle) => handle,
            Err(error) => return error,
        };
        *state = BeepState {
            initialized: true,
            handle,
            ready: false,
            stop: false,
            result: ERR_OK,
            mode: 0,
            frequency: 0,
            amplitude: 0.0,
            remaining_frames: 0,
        };
        handle
    };

    let worker = match thread::Builder::new()
        .name("wasapi-beep".to_string())
        .spawn(beep_worker)
    {
        Ok(worker) => worker,
        Err(_) => {
            if let Ok(mut state) = lock_beep_state() {
                state.initialized = false;
                state.handle = 0;
            }
            return ERR_INIT;
        }
    };

    match beep_controller().worker.lock() {
        Ok(mut slot) => *slot = Some(worker),
        Err(_) => return ERR_INIT,
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut state = match lock_beep_state() {
        Ok(state) => state,
        Err(error) => return error,
    };
    while !state.ready {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let wait = deadline.saturating_duration_since(now);
        match beep_controller().changed.wait_timeout(state, wait) {
            Ok((next, timeout)) => {
                state = next;
                if timeout.timed_out() && !state.ready {
                    break;
                }
            }
            Err(_) => return ERR_INIT,
        }
    }

    let result = if state.ready { state.result } else { ERR_INIT };
    if result == ERR_OK {
        *out_handle = handle as *mut c_void;
        return ERR_OK;
    }
    state.stop = true;
    beep_controller().changed.notify_all();
    drop(state);

    if let Ok(mut slot) = beep_controller().worker.lock() {
        if let Some(worker) = slot.take() {
            if worker.join().is_err() {
                return ERR_INIT;
            }
        }
    }
    if let Ok(mut state) = lock_beep_state() {
        state.initialized = false;
        state.handle = 0;
    }
    result
}

#[no_mangle]
pub extern "C" fn wasapi_beep(
    handle: *mut c_void,
    duration_ms: i32,
    frequency_hz: i32,
    amplitude: f64,
) -> i32 {
    let mut state = match lock_beep_state() {
        Ok(state) => state,
        Err(error) => return error,
    };
    if !has_beep_handle(&state, handle) {
        return ERR_BAD_HANDLE;
    }
    if frequency_hz <= 0
        || frequency_hz >= BEEP_SAMPLE_RATE / 2
        || duration_ms <= 0
        || duration_ms > 60_000
        || !(0.0..=1.0).contains(&amplitude)
    {
        return ERR_BAD_ARGS;
    }

    if state.result != ERR_OK {
        return state.result;
    }
    if state.mode != 0 {
        return ERR_BAD_ARGS;
    }

    state.mode = 3;
    state.frequency = frequency_hz;
    state.amplitude = amplitude;
    state.remaining_frames =
        (BEEP_SAMPLE_RATE as i64 * duration_ms as i64) / 1000;
    state.mode = 2;
    beep_controller().changed.notify_all();

    let deadline = Instant::now() + Duration::from_millis(duration_ms as u64 + 5000);
    loop {
        if state.result != ERR_OK {
            return state.result;
        }
        if state.mode != 2 {
            return ERR_OK;
        }
        if state.stop {
            return ERR_WRITE;
        }
        let now = Instant::now();
        if now >= deadline {
            state.mode = 0;
            state.frequency = 0;
            state.remaining_frames = 0;
            beep_controller().changed.notify_all();
            return ERR_WRITE;
        }
        let wait = deadline.saturating_duration_since(now);
        match beep_controller().changed.wait_timeout(state, wait) {
            Ok((next, timeout)) => {
                state = next;
                if timeout.timed_out() && state.mode == 2 {
                    state.mode = 0;
                    state.frequency = 0;
                    state.remaining_frames = 0;
                    beep_controller().changed.notify_all();
                    return ERR_WRITE;
                }
            }
            Err(_) => return ERR_INIT,
        }
    }
}

#[no_mangle]
pub extern "C" fn wasapi_beep_on(
    handle: *mut c_void,
    frequency_hz: i32,
    amplitude: f64,
) -> i32 {
    let mut state = match lock_beep_state() {
        Ok(state) => state,
        Err(error) => return error,
    };
    if !has_beep_handle(&state, handle) {
        return ERR_BAD_HANDLE;
    }
    if frequency_hz <= 0
        || frequency_hz >= BEEP_SAMPLE_RATE / 2
        || !(0.0..=1.0).contains(&amplitude)
    {
        return ERR_BAD_ARGS;
    }
    if state.result != ERR_OK {
        return state.result;
    }
    if state.mode == 3 {
        return ERR_BAD_ARGS;
    }
    state.amplitude = amplitude;
    state.frequency = frequency_hz;
    state.remaining_frames = 0;
    state.mode = 1;
    beep_controller().changed.notify_all();
    ERR_OK
}

#[no_mangle]
pub extern "C" fn wasapi_beep_off(handle: *mut c_void) -> i32 {
    let mut state = match lock_beep_state() {
        Ok(state) => state,
        Err(error) => return error,
    };
    if !has_beep_handle(&state, handle) {
        return ERR_BAD_HANDLE;
    }
    if state.result != ERR_OK {
        return state.result;
    }
    state.mode = 0;
    state.frequency = 0;
    state.remaining_frames = 0;
    beep_controller().changed.notify_all();
    ERR_OK
}

#[no_mangle]
pub extern "C" fn wasapi_beep_close(handle: *mut c_void) -> i32 {
    {
        let mut state = match lock_beep_state() {
            Ok(state) => state,
            Err(error) => return error,
        };
        if !has_beep_handle(&state, handle) {
            return ERR_BAD_HANDLE;
        }
        state.handle = 0;
        state.stop = true;
        state.mode = 0;
        state.frequency = 0;
        state.remaining_frames = 0;
        beep_controller().changed.notify_all();
    }

    let worker_result = match beep_controller().worker.lock() {
        Ok(mut slot) => match slot.take() {
            Some(worker) => {
                if worker.join().is_err() {
                    ERR_INIT
                } else {
                    ERR_OK
                }
            }
            None => ERR_INIT,
        },
        Err(_) => ERR_INIT,
    };

    let mut state = match lock_beep_state() {
        Ok(state) => state,
        Err(error) => return error,
    };
    let result = if state.result != ERR_OK {
        state.result
    } else {
        worker_result
    };
    state.initialized = false;
    state.ready = false;
    state.stop = false;
    state.mode = 0;
    state.frequency = 0;
    state.remaining_frames = 0;
    result
}

#[cfg(test)]
mod tests {
    use super::{has_beep_handle, BeepState, ERR_OK};
    use std::ptr;

    #[test]
    fn beep_handle_must_match_an_active_session() {
        let state = BeepState {
            initialized: true,
            handle: 17,
            ready: true,
            stop: false,
            result: ERR_OK,
            mode: 0,
            frequency: 0,
            amplitude: 0.0,
            remaining_frames: 0,
        };

        assert!(has_beep_handle(&state, 17usize as *mut _));
        assert!(!has_beep_handle(&state, 18usize as *mut _));
        assert!(!has_beep_handle(&state, ptr::null_mut()));

        let closed_state = BeepState {
            initialized: false,
            ..state
        };
        assert!(!has_beep_handle(&closed_state, 17usize as *mut _));
    }
}
