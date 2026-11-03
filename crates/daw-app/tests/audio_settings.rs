//! SPEC-033: audio device settings, tested without a sound card.

use daw_app::audio_settings::{AudioSettings, BUFFER_SIZES, buffer_ms, choose_buffer, find_device};

fn full() -> AudioSettings {
    AudioSettings {
        output: Some("USB Audio CODEC".into()),
        input: Some("Built-in Mic".into()),
        buffer_frames: Some(256),
        interpolation: audio_core::Interpolation::Hermite4,
    }
}

#[test]
fn defaults_mean_system_devices() {
    let s = AudioSettings::default();
    assert_eq!(s.output, None);
    assert_eq!(s.input, None);
    assert_eq!(s.buffer_frames, None);
}

#[test]
fn text_round_trips() {
    let s = full();
    assert_eq!(AudioSettings::parse(&s.to_text()), s);
    let empty = AudioSettings::default();
    assert_eq!(AudioSettings::parse(&empty.to_text()), empty);
}

#[test]
fn device_names_with_equals_and_spaces_survive() {
    let s = AudioSettings {
        output: Some("hw:CARD=PCH,DEV=0".into()),
        ..AudioSettings::default()
    };
    assert_eq!(AudioSettings::parse(&s.to_text()), s);
}

#[test]
fn newlines_in_names_cannot_inject_keys() {
    let s = AudioSettings {
        output: Some("evil\nbuffer = 64".into()),
        ..AudioSettings::default()
    };
    let back = AudioSettings::parse(&s.to_text());
    assert_eq!(back.buffer_frames, None);
    assert_eq!(back.output.as_deref(), Some("evil buffer = 64"));
}

#[test]
fn damaged_file_falls_back_to_defaults() {
    let s = AudioSettings::parse("garbage\nbuffer = lots\nbuffer = 333\nunknown = 1\noutput =\n");
    assert_eq!(s, AudioSettings::default());
}

#[test]
fn only_offered_buffer_sizes_are_accepted() {
    for n in BUFFER_SIZES {
        let s = AudioSettings::parse(&format!("buffer = {n}"));
        assert_eq!(s.buffer_frames, Some(n));
    }
    assert_eq!(AudioSettings::parse("buffer = 0").buffer_frames, None);
}

#[test]
fn save_and_load_through_a_file() {
    let dir = std::env::temp_dir().join(format!("sound-pro-033-{}", std::process::id()));
    let path = dir.join("nested").join("audio.conf");
    let s = full();
    s.save(&path).unwrap();
    assert_eq!(AudioSettings::load(&path), s);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn missing_file_loads_defaults() {
    let path = std::env::temp_dir().join("sound-pro-033-does-not-exist/audio.conf");
    assert_eq!(AudioSettings::load(&path), AudioSettings::default());
}

#[test]
fn find_device_by_name_or_default() {
    let names = vec!["Default".to_owned(), "USB".to_owned(), "HDMI".to_owned()];
    assert_eq!(find_device(&names, Some("USB")), Some(1));
    assert_eq!(find_device(&names, Some("HDMI")), Some(2));
    // Unplugged device: fall back to the system default.
    assert_eq!(find_device(&names, Some("Gone")), None);
    assert_eq!(find_device(&names, None), None);
    assert_eq!(find_device(&[], Some("USB")), None);
}

#[test]
fn buffer_is_clamped_to_the_device_range() {
    assert_eq!(choose_buffer(None, Some((64, 4096))), None);
    assert_eq!(choose_buffer(Some(256), Some((64, 4096))), Some(256));
    assert_eq!(choose_buffer(Some(64), Some((128, 4096))), Some(128));
    assert_eq!(choose_buffer(Some(2048), Some((64, 1024))), Some(1024));
    assert_eq!(choose_buffer(Some(512), None), Some(512));
    // A nonsense range from a driver is ignored rather than trusted.
    assert_eq!(choose_buffer(Some(512), Some((1024, 64))), Some(512));
}

#[test]
fn buffer_latency_in_milliseconds() {
    assert!((buffer_ms(480, 48_000) - 10.0).abs() < 1e-9);
    assert!((buffer_ms(256, 44_100) - 5.804_988_662).abs() < 1e-6);
    assert!(buffer_ms(256, 0).abs() < 1e-12);
}

#[test]
fn interpolation_defaults_to_linear_and_round_trips() {
    use audio_core::Interpolation;
    assert_eq!(
        AudioSettings::default().interpolation,
        Interpolation::Linear
    );
    let s = AudioSettings {
        interpolation: Interpolation::Hermite4,
        ..AudioSettings::default()
    };
    assert!(s.to_text().contains("interpolation = hermite"));
    assert_eq!(AudioSettings::parse(&s.to_text()), s);
    let linear = AudioSettings::default();
    assert_eq!(AudioSettings::parse(&linear.to_text()), linear);
}

#[test]
fn invalid_interpolation_falls_back_to_linear() {
    use audio_core::Interpolation;
    for text in [
        "interpolation = cubic\n",
        "interpolation = \n",
        "interpolation = HERMITE9\n",
    ] {
        assert_eq!(
            AudioSettings::parse(text).interpolation,
            Interpolation::Linear
        );
    }
}
