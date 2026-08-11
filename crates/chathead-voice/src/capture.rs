use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
};

use chathead_core::VoiceInputDevice;
use cpal::{
    Device, DeviceDirection, Host, HostId, SampleFormat, Stream, StreamConfig,
    SupportedStreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};

const MAX_CAPTURE_SECONDS: usize = 31;

#[derive(Clone, Debug, PartialEq)]
pub struct CapturedAudio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub overflowed: bool,
    pub device_lost: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("no microphone input device is available")]
    NoMicrophone,
    #[error("the selected microphone is unavailable")]
    DeviceUnavailable,
    #[error("speaker monitor and loopback devices cannot be used as microphones")]
    LoopbackRejected,
    #[error("audio server is unavailable: {0}")]
    AudioServer(String),
    #[error("microphone access failed: {0}")]
    Access(String),
    #[error("microphone sample format {0} is unsupported")]
    UnsupportedFormat(SampleFormat),
    #[error("microphone capture is not running")]
    NotRunning,
}

pub trait VoiceCapture {
    fn start(&mut self, input_device_id: Option<&str>) -> Result<(), CaptureError>;
    fn drain_available(&mut self, output: &mut Vec<f32>) -> Result<(), CaptureError>;
    fn stop(&mut self) -> Result<CapturedAudio, CaptureError>;
    fn sample_rate(&self) -> Option<u32>;
}

/// Opens and immediately closes an input stream so device changes can be
/// validated without blocking the voice command worker.
pub fn probe_input_device(input_device_id: Option<&str>) -> Result<(), CaptureError> {
    let mut capture = AudioCapture::default();
    capture.start(input_device_id)?;
    capture.stop().map(|_| ())
}

#[derive(Default)]
pub struct AudioCapture {
    stream: Option<Stream>,
    buffer: Option<SpscF32>,
    sample_rate: Option<u32>,
    overflowed: Arc<AtomicBool>,
    device_lost: Arc<AtomicBool>,
}

pub fn discover_input_devices() -> Result<Vec<VoiceInputDevice>, CaptureError> {
    let mut last_error = None;
    for host_id in preferred_host_ids() {
        let host = match cpal::host_from_id(host_id) {
            Ok(host) => host,
            Err(error) => {
                last_error = Some(error.to_string());
                continue;
            }
        };
        match discover_host_input_devices(&host, host_id) {
            Ok(devices) if !devices.is_empty() => return Ok(devices),
            Ok(_) => {}
            Err(error) => last_error = Some(error.to_string()),
        }
    }
    last_error.map_or(Ok(Vec::new()), |error| {
        Err(CaptureError::AudioServer(error))
    })
}

fn discover_host_input_devices(
    host: &Host,
    host_id: HostId,
) -> Result<Vec<VoiceInputDevice>, CaptureError> {
    let default_id = host
        .default_input_device()
        .and_then(|device| device.id().ok())
        .map(|id| id.to_string());
    let devices = host
        .input_devices()
        .map_err(|error| CaptureError::AudioServer(error.to_string()))?;
    let mut result = Vec::new();

    for device in devices {
        let Ok(id) = device.id() else {
            continue;
        };
        let id = id.to_string();
        let description = device.description().ok();
        let name = description
            .as_ref()
            .map_or_else(|| device.to_string(), |value| value.name().to_owned());
        let direction = description.as_ref().map(|value| value.direction());
        if !is_user_facing_input(host_id, id.as_str(), name.as_str(), direction) {
            continue;
        }
        let normalized_name = normalized_device_name(name.as_str());
        if result.iter().any(|known: &VoiceInputDevice| {
            normalized_device_name(known.name.as_str()) == normalized_name
        }) {
            continue;
        }
        result.push(VoiceInputDevice {
            is_default: default_id.as_deref() == Some(id.as_str()),
            id,
            name,
        });
    }

    result.sort_by(|left, right| {
        right
            .is_default
            .cmp(&left.is_default)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    Ok(result)
}

fn is_user_facing_input(
    host_id: HostId,
    id: &str,
    name: &str,
    direction: Option<DeviceDirection>,
) -> bool {
    if is_loopback_name(name) || is_loopback_name(id) {
        return false;
    }
    if host_id != HostId::PipeWire {
        return true;
    }

    let backend_id = id
        .split_once(':')
        .map_or(id, |(_, backend_id)| backend_id)
        .to_ascii_lowercase();
    let normalized_name = normalized_device_name(name);
    !matches!(
        backend_id.as_str(),
        "sink_default" | "input_default" | "output_default" | "unknown"
    ) && normalized_name != "unknown"
        && direction == Some(DeviceDirection::Input)
}

fn normalized_device_name(name: &str) -> String {
    name.trim().to_lowercase()
}

impl VoiceCapture for AudioCapture {
    fn start(&mut self, input_device_id: Option<&str>) -> Result<(), CaptureError> {
        if self.stream.is_some() {
            return Err(CaptureError::Access("capture is already active".to_owned()));
        }
        let (device, config) = resolve_device(input_device_id)?;
        if is_loopback_name(&device.to_string()) {
            return Err(CaptureError::LoopbackRejected);
        }
        let sample_rate = config.sample_rate();
        let channels = usize::from(config.channels());
        let capacity = usize::try_from(sample_rate)
            .unwrap_or(48_000)
            .saturating_mul(MAX_CAPTURE_SECONDS)
            .max(1);
        let buffer = SpscF32::new(capacity);
        self.overflowed.store(false, Ordering::Relaxed);
        self.device_lost.store(false, Ordering::Relaxed);
        let stream = build_input_stream(
            &device,
            &config,
            channels,
            buffer.clone(),
            Arc::clone(&self.overflowed),
            Arc::clone(&self.device_lost),
        )?;
        stream
            .play()
            .map_err(|error| CaptureError::Access(error.to_string()))?;
        self.sample_rate = Some(sample_rate);
        self.buffer = Some(buffer);
        self.stream = Some(stream);
        Ok(())
    }

    fn drain_available(&mut self, output: &mut Vec<f32>) -> Result<(), CaptureError> {
        let buffer = self.buffer.as_ref().ok_or(CaptureError::NotRunning)?;
        while let Some(sample) = buffer.pop() {
            output.push(sample);
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<CapturedAudio, CaptureError> {
        drop(self.stream.take().ok_or(CaptureError::NotRunning)?);
        let mut samples = Vec::new();
        self.drain_available(&mut samples)?;
        self.buffer = None;
        Ok(CapturedAudio {
            samples,
            sample_rate: self.sample_rate.take().unwrap_or(16_000),
            overflowed: self.overflowed.load(Ordering::Relaxed),
            device_lost: self.device_lost.load(Ordering::Relaxed),
        })
    }

    fn sample_rate(&self) -> Option<u32> {
        self.sample_rate
    }
}

fn preferred_host_ids() -> [HostId; 3] {
    [HostId::PipeWire, HostId::PulseAudio, HostId::Alsa]
}

fn resolve_device(selected: Option<&str>) -> Result<(Device, SupportedStreamConfig), CaptureError> {
    let mut last_error = None;
    for host_id in preferred_host_ids() {
        let host = match cpal::host_from_id(host_id) {
            Ok(host) => host,
            Err(error) => {
                last_error = Some(error.to_string());
                continue;
            }
        };
        let device = if let Some(selected) = selected {
            match find_selected(&host, selected)? {
                Some(device) => device,
                None => continue,
            }
        } else {
            match host
                .default_input_device()
                .filter(|device| !is_loopback_name(&device.to_string()))
            {
                Some(device) => device,
                None => continue,
            }
        };
        let config = device
            .default_input_config()
            .map_err(|error| CaptureError::Access(error.to_string()))?;
        return Ok((device, config));
    }
    if selected.is_some() {
        Err(CaptureError::DeviceUnavailable)
    } else if let Some(error) = last_error {
        Err(CaptureError::AudioServer(error))
    } else {
        Err(CaptureError::NoMicrophone)
    }
}

fn find_selected(host: &Host, selected: &str) -> Result<Option<Device>, CaptureError> {
    let devices = host
        .input_devices()
        .map_err(|error| CaptureError::AudioServer(error.to_string()))?;
    for device in devices {
        if device
            .id()
            .ok()
            .is_some_and(|id| id.to_string() == selected)
        {
            if is_loopback_name(&device.to_string()) {
                return Err(CaptureError::LoopbackRejected);
            }
            return Ok(Some(device));
        }
    }
    Ok(None)
}

fn is_loopback_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "monitor of",
        ".monitor",
        "loopback",
        "stereo mix",
        "what u hear",
    ]
    .iter()
    .any(|marker| name.contains(marker))
}

fn build_input_stream(
    device: &Device,
    config: &SupportedStreamConfig,
    channels: usize,
    buffer: SpscF32,
    overflowed: Arc<AtomicBool>,
    device_lost: Arc<AtomicBool>,
) -> Result<Stream, CaptureError> {
    let stream_config: StreamConfig = (*config).into();
    match config.sample_format() {
        SampleFormat::F32 => build_typed_stream::<f32>(
            device,
            &stream_config,
            channels,
            buffer,
            overflowed,
            device_lost,
        ),
        SampleFormat::I16 => build_typed_stream::<i16>(
            device,
            &stream_config,
            channels,
            buffer,
            overflowed,
            device_lost,
        ),
        SampleFormat::U16 => build_typed_stream::<u16>(
            device,
            &stream_config,
            channels,
            buffer,
            overflowed,
            device_lost,
        ),
        SampleFormat::I32 => build_typed_stream::<i32>(
            device,
            &stream_config,
            channels,
            buffer,
            overflowed,
            device_lost,
        ),
        SampleFormat::I8 => build_typed_stream::<i8>(
            device,
            &stream_config,
            channels,
            buffer,
            overflowed,
            device_lost,
        ),
        SampleFormat::U8 => build_typed_stream::<u8>(
            device,
            &stream_config,
            channels,
            buffer,
            overflowed,
            device_lost,
        ),
        format => Err(CaptureError::UnsupportedFormat(format)),
    }
}

fn build_typed_stream<T: cpal::SizedSample + SampleToF32>(
    device: &Device,
    config: &StreamConfig,
    channels: usize,
    buffer: SpscF32,
    overflowed: Arc<AtomicBool>,
    device_lost: Arc<AtomicBool>,
) -> Result<Stream, CaptureError> {
    let error_flag = Arc::clone(&device_lost);
    device
        .build_input_stream(
            *config,
            move |samples: &[T], _| {
                for frame in samples.chunks_exact(channels) {
                    let mono = frame
                        .iter()
                        .fold(0.0_f32, |sum, sample| sum + sample.to_f32())
                        / channels as f32;
                    if !buffer.push(mono) {
                        overflowed.store(true, Ordering::Relaxed);
                    }
                }
            },
            move |_| error_flag.store(true, Ordering::Relaxed),
            None,
        )
        .map_err(|error| CaptureError::Access(error.to_string()))
}

trait SampleToF32: Copy + Send + 'static {
    fn to_f32(self) -> f32;
}

impl SampleToF32 for f32 {
    fn to_f32(self) -> f32 {
        self
    }
}
impl SampleToF32 for i16 {
    fn to_f32(self) -> f32 {
        f32::from(self) / f32::from(i16::MAX)
    }
}
impl SampleToF32 for u16 {
    fn to_f32(self) -> f32 {
        (f32::from(self) / f32::from(u16::MAX)).mul_add(2.0, -1.0)
    }
}
impl SampleToF32 for i32 {
    fn to_f32(self) -> f32 {
        self as f32 / i32::MAX as f32
    }
}
impl SampleToF32 for i8 {
    fn to_f32(self) -> f32 {
        f32::from(self) / f32::from(i8::MAX)
    }
}
impl SampleToF32 for u8 {
    fn to_f32(self) -> f32 {
        (f32::from(self) / f32::from(u8::MAX)).mul_add(2.0, -1.0)
    }
}

#[derive(Clone)]
struct SpscF32(Arc<SpscInner>);

struct SpscInner {
    slots: Box<[AtomicU32]>,
    capacity: usize,
    read: AtomicUsize,
    write: AtomicUsize,
}

impl SpscF32 {
    fn new(capacity: usize) -> Self {
        let slots = (0..capacity).map(|_| AtomicU32::new(0)).collect();
        Self(Arc::new(SpscInner {
            slots,
            capacity,
            read: AtomicUsize::new(0),
            write: AtomicUsize::new(0),
        }))
    }

    fn push(&self, value: f32) -> bool {
        let write = self.0.write.load(Ordering::Relaxed);
        let next = (write + 1) % self.0.capacity;
        if next == self.0.read.load(Ordering::Acquire) {
            return false;
        }
        self.0.slots[write].store(value.to_bits(), Ordering::Relaxed);
        self.0.write.store(next, Ordering::Release);
        true
    }

    fn pop(&self) -> Option<f32> {
        let read = self.0.read.load(Ordering::Relaxed);
        if read == self.0.write.load(Ordering::Acquire) {
            return None;
        }
        let value = f32::from_bits(self.0.slots[read].load(Ordering::Relaxed));
        self.0
            .read
            .store((read + 1) % self.0.capacity, Ordering::Release);
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_integer_samples_to_normalized_float() {
        assert!((i16::MAX.to_f32() - 1.0).abs() < 0.0001);
        assert!((0_u16.to_f32() + 1.0).abs() < 0.0001);
        assert!((u16::MAX.to_f32() - 1.0).abs() < 0.0001);
    }

    #[test]
    fn bounded_spsc_reports_overflow_without_overwriting() {
        let buffer = SpscF32::new(3);
        assert!(buffer.push(1.0));
        assert!(buffer.push(2.0));
        assert!(!buffer.push(3.0));
        assert_eq!(buffer.pop(), Some(1.0));
        assert_eq!(buffer.pop(), Some(2.0));
        assert_eq!(buffer.pop(), None);
    }

    #[test]
    fn excludes_monitor_and_loopback_devices() {
        assert!(is_loopback_name("Monitor of Built-in Audio"));
        assert!(is_loopback_name("alsa_output.monitor"));
        assert!(!is_loopback_name("USB Microphone"));
    }

    #[test]
    fn pipewire_picker_excludes_default_aliases_outputs_and_unknown_nodes() {
        assert!(!is_user_facing_input(
            HostId::PipeWire,
            "pipewire:sink_default",
            "sink_default",
            Some(DeviceDirection::Duplex)
        ));
        assert!(!is_user_facing_input(
            HostId::PipeWire,
            "pipewire:input_default",
            "input_default",
            Some(DeviceDirection::Input)
        ));
        assert!(!is_user_facing_input(
            HostId::PipeWire,
            "pipewire:alsa_output.usb-headset.analog-stereo",
            "USB Headset",
            Some(DeviceDirection::Duplex)
        ));
        assert!(!is_user_facing_input(
            HostId::PipeWire,
            "pipewire:unknown",
            "unknown",
            Some(DeviceDirection::Input)
        ));
        assert!(is_user_facing_input(
            HostId::PipeWire,
            "pipewire:alsa_input.usb-headset.mono-fallback",
            "AULA-G7",
            Some(DeviceDirection::Input)
        ));
    }

    #[test]
    fn compatibility_hosts_still_accept_real_input_devices() {
        assert!(is_user_facing_input(
            HostId::PulseAudio,
            "pulseaudio:alsa_input.usb-headset.mono-fallback",
            "AULA-G7",
            Some(DeviceDirection::Input)
        ));
        assert!(is_user_facing_input(
            HostId::Alsa,
            "alsa:hw:1,0",
            "USB Microphone",
            Some(DeviceDirection::Duplex)
        ));
    }
}
