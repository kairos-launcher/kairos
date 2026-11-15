//! Output volume of the default device: Linux `wpctl` (PipeWire) or `pactl` (PulseAudio),
//! Windows Core Audio, macOS AppleScript.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Volume {
    pub percent: u8,
    pub muted: bool,
}

pub fn get() -> Option<Volume> {
    imp::get()
}

pub fn set(percent: u8) -> Result<(), String> {
    imp::set(percent.min(100))
}

pub fn set_muted(muted: bool) -> Result<(), String> {
    imp::set_muted(muted)
}

/// `Volume: 0.40 [MUTED]` from `wpctl get-volume`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_wpctl(s: &str) -> Option<Volume> {
    let rest = s.trim().strip_prefix("Volume:")?.trim();
    let level: f64 = rest.split_whitespace().next()?.parse().ok()?;
    Some(Volume { percent: percent(level), muted: rest.contains("[MUTED]") })
}

/// The first channel of `pactl get-sink-volume` (`Volume: front-left: 26214 /  40% / …`) and
/// `pactl get-sink-mute` (`Mute: yes`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_pactl(volume: &str, mute: &str) -> Option<Volume> {
    let pct = volume.split('/').find_map(|part| part.trim().strip_suffix('%')?.trim().parse::<u16>().ok())?;
    Some(Volume { percent: u8::try_from(pct.min(100)).unwrap_or(100), muted: mute.trim().ends_with("yes") })
}

/// 0.0–1.0 → 0–100.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn percent(level: f64) -> u8 {
    (level.clamp(0.0, 1.0) * 100.0).round() as u8
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{Volume, parse_pactl, parse_wpctl};
    use crate::run;

    const SINK: &str = "@DEFAULT_AUDIO_SINK@";
    const PA_SINK: &str = "@DEFAULT_SINK@";

    fn wpctl() -> bool {
        run::has("wpctl")
    }

    pub fn get() -> Option<Volume> {
        if wpctl() {
            return parse_wpctl(&run::output(&["wpctl", "get-volume", SINK])?);
        }
        let v = run::output(&["pactl", "get-sink-volume", PA_SINK])?;
        let m = run::output(&["pactl", "get-sink-mute", PA_SINK])?;
        parse_pactl(&v, &m)
    }

    pub fn set(percent: u8) -> Result<(), String> {
        if wpctl() {
            let level = format!("{:.2}", f64::from(percent) / 100.0);
            return run::status(&["wpctl", "set-volume", SINK, &level]);
        }
        run::status(&["pactl", "set-sink-volume", PA_SINK, &format!("{percent}%")])
    }

    pub fn set_muted(muted: bool) -> Result<(), String> {
        let flag = if muted { "1" } else { "0" };
        if wpctl() {
            return run::status(&["wpctl", "set-mute", SINK, flag]);
        }
        run::status(&["pactl", "set-sink-mute", PA_SINK, flag])
    }
}

#[cfg(windows)]
mod imp {
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{IMMDeviceEnumerator, MMDeviceEnumerator, eConsole, eRender};
    use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};

    use super::{Volume, percent};

    fn endpoint() -> windows::core::Result<IAudioEndpointVolume> {
        // SAFETY: COM calls on this thread after initialising it; the interfaces are reference
        // counted by the `windows` crate.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let e: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            e.GetDefaultAudioEndpoint(eRender, eConsole)?.Activate(CLSCTX_ALL, None)
        }
    }

    pub fn get() -> Option<Volume> {
        let v = endpoint().ok()?;
        // SAFETY: plain getters on a valid interface.
        unsafe {
            let level = v.GetMasterVolumeLevelScalar().ok()?;
            let muted = v.GetMute().ok()?.as_bool();
            Some(Volume { percent: percent(f64::from(level)), muted })
        }
    }

    pub fn set(pct: u8) -> Result<(), String> {
        let v = endpoint().map_err(|e| format!("audio device: {e}"))?;
        // SAFETY: a null event context is allowed.
        unsafe { v.SetMasterVolumeLevelScalar(f32::from(pct) / 100.0, std::ptr::null()) }.map_err(|e| e.to_string())
    }

    pub fn set_muted(muted: bool) -> Result<(), String> {
        let v = endpoint().map_err(|e| format!("audio device: {e}"))?;
        // SAFETY: a null event context is allowed.
        unsafe { v.SetMute(muted, std::ptr::null()) }.map_err(|e| e.to_string())
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::Volume;
    use crate::run;

    fn osa(script: &str) -> Option<String> {
        run::output(&["osascript", "-e", script])
    }

    pub fn get() -> Option<Volume> {
        let percent = osa("output volume of (get volume settings)")?.parse().ok()?;
        let muted = osa("output muted of (get volume settings)")? == "true";
        Some(Volume { percent, muted })
    }

    pub fn set(percent: u8) -> Result<(), String> {
        run::status(&["osascript", "-e", &format!("set volume output volume {percent}")])
    }

    pub fn set_muted(muted: bool) -> Result<(), String> {
        run::status(&["osascript", "-e", &format!("set volume output muted {muted}")])
    }
}

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
mod imp {
    use super::Volume;

    pub fn get() -> Option<Volume> {
        None
    }

    pub fn set(_: u8) -> Result<(), String> {
        Err("not supported on this system".into())
    }

    pub fn set_muted(_: bool) -> Result<(), String> {
        Err("not supported on this system".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wpctl_output() {
        assert_eq!(parse_wpctl("Volume: 0.40"), Some(Volume { percent: 40, muted: false }));
        assert_eq!(parse_wpctl("Volume: 0.88 [MUTED]\n"), Some(Volume { percent: 88, muted: true }));
        assert_eq!(parse_wpctl("Volume: 1.50"), Some(Volume { percent: 100, muted: false }), "boosted");
        assert_eq!(parse_wpctl("nope"), None);
    }

    #[test]
    fn pactl_output() {
        let v = "Volume: front-left: 26214 /  40% / -23.88 dB,   front-right: 26214 /  40% / -23.88 dB\n        balance 0.00";
        assert_eq!(parse_pactl(v, "Mute: no"), Some(Volume { percent: 40, muted: false }));
        assert_eq!(parse_pactl(v, "Mute: yes"), Some(Volume { percent: 40, muted: true }));
        assert_eq!(parse_pactl("garbage", "Mute: no"), None);
    }
}
