//! SFZ parser tests (SPEC-062).
//!
//! Drives the pure [`parse`] function against small hand-written fixtures and
//! inline sources. No audio hardware, no sample files, no realtime path.

use std::collections::HashMap;

use audio_core::sampler::sfz::{
    IncludeResolver, LoopMode, NO_KEY, OffMode, Region, SfzDocument, SfzError, Trigger, parse,
};

/// A test resolver backed by an in-memory map of path → text.
#[derive(Default)]
struct MapResolver {
    files: HashMap<String, String>,
}

impl IncludeResolver for MapResolver {
    fn resolve(&mut self, path: &str) -> Result<String, String> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| format!("no such include: {path}"))
    }
}

fn parse_text(text: &str) -> Result<SfzDocument, SfzError> {
    let mut resolver = MapResolver::default();
    parse(text, &mut resolver)
}

fn parse_with(text: &str, resolver: &mut MapResolver) -> Result<SfzDocument, SfzError> {
    parse(text, resolver)
}

/// `key=60` must set `lokey`, `hikey` and `pitch_keycenter` to the same value.
#[test]
fn key_sets_all_three() {
    let doc = parse_text("<region>\nsample=x.wav\nkey=60\n").expect("parse");
    let r = &doc.regions[0];
    assert_eq!(r.lokey, Some(60));
    assert_eq!(r.hikey, Some(60));
    assert_eq!(r.pitch_keycenter, Some(60));
    assert_eq!(r.sample.as_deref(), Some("x.wav"));
}

/// `-1` is SFZ's "no note" sentinel: it must parse (not error) and be mapped
/// to [`NO_KEY`], so a keyless release/pedal group loads but matches nothing.
#[test]
fn key_minus_one_is_no_key() {
    let text = "<group>\ngroup=1\nhikey=-1\nlokey=-1\n<region>\nsample=release.wav\n";
    let doc = parse_text(text).expect("-1 keys parse without error");
    assert_eq!(doc.regions.len(), 1);
    let r = &doc.regions[0];
    assert_eq!(r.lokey, Some(NO_KEY));
    assert_eq!(r.hikey, Some(NO_KEY));
    assert!(doc.warnings.is_empty());
}

/// `key=-1` maps all three key fields to [`NO_KEY`] and still parses.
#[test]
fn key_minus_one_sets_all_three() {
    let doc = parse_text("<region>\nsample=release.wav\nkey=-1\n").expect("parse");
    let r = &doc.regions[0];
    assert_eq!(r.lokey, Some(NO_KEY));
    assert_eq!(r.hikey, Some(NO_KEY));
    assert_eq!(r.pitch_keycenter, Some(NO_KEY));
}

/// `<control>` `default_path` is captured, regions inherit global/group values.
#[test]
fn fixture_basic_inheritance() {
    let text = include_str!("fixtures/basic.sfz");
    let doc = parse_text(text).expect("parse basic.sfz");
    assert_eq!(doc.default_path.as_deref(), Some("Samples/"));
    assert_eq!(doc.regions.len(), 2);

    let r0 = &doc.regions[0];
    assert_eq!(r0.sample.as_deref(), Some("piano_c4.wav"));
    // Inherited from <global>.
    assert_eq!(r0.volume, Some(-6.0));
    assert_eq!(r0.amp_veltrack, Some(100.0));
    // Inherited from <group>.
    assert_eq!(r0.group, Some(1));
    assert_eq!(r0.off_by, Some(1));

    let r1 = &doc.regions[1];
    assert_eq!(r1.sample.as_deref(), Some("piano_c5.wav"));
    assert_eq!(r1.lokey, Some(61));
    assert_eq!(r1.hikey, Some(72));
    assert_eq!(r1.pitch_keycenter, Some(64));
    assert_eq!(r1.hivel, Some(64));
    // Still inherited.
    assert_eq!(r1.volume, Some(-6.0));
    assert_eq!(r1.group, Some(1));
}

/// `#define` substitution and comment stripping.
#[test]
fn defines_and_comments() {
    let text = include_str!("fixtures/defines.sfz");
    let doc = parse_text(text).expect("parse defines.sfz");
    assert_eq!(doc.regions.len(), 1);
    let r = &doc.regions[0];
    assert_eq!(r.sample.as_deref(), Some("included.wav"));
    assert_eq!(r.volume, Some(-3.0));
    assert_eq!(r.ampeg_attack, Some(0.01));
    assert_eq!(r.ampeg_sustain, Some(50.0));
    assert_eq!(r.loop_mode, Some(LoopMode::LoopContinuous));
    // Comments must not leak into any opcode.
    assert!(doc.warnings.is_empty());
}

/// Unknown header is skipped; unknown opcode warns but keeps the region.
#[test]
fn unknown_header_and_opcode() {
    let text = include_str!("fixtures/unknown.sfz");
    let doc = parse_text(text).expect("parse unknown.sfz");
    // <effect> is skipped; only the <region> survives.
    assert_eq!(doc.regions.len(), 1);
    let r = &doc.regions[0];
    assert_eq!(r.sample.as_deref(), Some("hat.wav"));
    assert_eq!(r.group, Some(2));
    assert_eq!(r.off_by, Some(2));
    assert_eq!(r.off_mode, Some(OffMode::Fast));
    assert_eq!(r.trigger, Some(Trigger::Attack));
    assert_eq!(r.seq_length, Some(3));
    assert_eq!(r.seq_position, Some(1));
    assert_eq!(r.lorand, Some(0.1));
    assert_eq!(r.hirand, Some(0.9));
    // `fil_type` is unknown → one warning, region kept.
    assert_eq!(doc.warnings.len(), 1);
    assert!(doc.warnings[0].message.contains("fil_type"));
}

/// `#include` resolves through the resolver and is expanded inline.
#[test]
fn include_is_expanded() {
    let mut resolver = MapResolver::default();
    resolver.files.insert(
        "child.sfz".to_string(),
        "<region>\nsample=child.wav\n".to_string(),
    );
    let text = "#include \"child.sfz\"\n<region>\nsample=parent.wav\n";
    let doc = parse_with(text, &mut resolver).expect("parse include");
    assert_eq!(doc.regions.len(), 2);
    assert_eq!(doc.regions[0].sample.as_deref(), Some("child.wav"));
    assert_eq!(doc.regions[1].sample.as_deref(), Some("parent.wav"));
}

/// `#define` with a boolean-style bare value and `$` reuse.
#[test]
fn define_bare_value() {
    let doc = parse_text("#define $loop 1\n<region>\nsample=x.wav\nloop_mode=loop_continuous\n")
        .expect("parse");
    assert_eq!(doc.regions.len(), 1);
}

/// Undefined `$var` warns and substitutes empty.
#[test]
fn undefined_variable_warns() {
    let doc = parse_text("<region>\nsample=$missing.wav\n").expect("parse");
    assert_eq!(doc.regions[0].sample.as_deref(), Some(".wav"));
    assert_eq!(doc.warnings.len(), 1);
    assert!(doc.warnings[0].message.contains("missing"));
}

/// A quoted sample path with spaces survives.
#[test]
fn quoted_path_with_spaces() {
    let doc = parse_text("<region>\nsample=\"my samples/hat open.wav\"\n").expect("parse");
    assert_eq!(
        doc.regions[0].sample.as_deref(),
        Some("my samples/hat open.wav")
    );
}

// --- Error cases: must return Err, never panic. ---

#[test]
fn unterminated_block_comment_errors() {
    let err = parse_text("/* no close").unwrap_err();
    assert!(matches!(err, SfzError::UnterminatedComment { .. }));
}

#[test]
fn unsafe_include_path_errors() {
    let mut resolver = MapResolver::default();
    let err = parse_with("#include \"../evil.sfz\"\n", &mut resolver).unwrap_err();
    assert!(matches!(err, SfzError::UnsafePath { .. }));

    let err = parse_with("#include \"/abs/evil.sfz\"\n", &mut resolver).unwrap_err();
    assert!(matches!(err, SfzError::UnsafePath { .. }));
}

#[test]
fn missing_include_reports_resolver_error() {
    let mut resolver = MapResolver::default();
    let err = parse_with("#include \"nope.sfz\"\n", &mut resolver).unwrap_err();
    assert!(matches!(err, SfzError::Include { .. }));
}

#[test]
fn invalid_number_errors() {
    let err = parse_text("<region>\nkey=abc\n").unwrap_err();
    assert!(matches!(err, SfzError::InvalidNumber { .. }));
}

#[test]
fn lovel_out_of_range_errors() {
    let err = parse_text("<region>\nlovel=0\n").unwrap_err();
    assert!(matches!(err, SfzError::InvalidRange { .. }));

    let err = parse_text("<region>\nhivel=128\n").unwrap_err();
    assert!(matches!(err, SfzError::InvalidRange { .. }));
}

#[test]
fn malformed_include_errors() {
    let mut resolver = MapResolver::default();
    let err = parse_with("#include\n", &mut resolver).unwrap_err();
    assert!(matches!(err, SfzError::MalformedInclude { .. }));
}

/// Empty input parses to an empty document.
#[test]
fn empty_input_is_ok() {
    let doc = parse_text("").expect("empty parse");
    assert!(doc.regions.is_empty());
    assert!(doc.warnings.is_empty());
    assert_eq!(doc.default_path, None);
}

/// All v1 opcodes parse into their typed fields.
#[test]
fn full_v1_opcode_subset() {
    let text = "\
<region>
sample=x.wav
lokey=36
hikey=48
pitch_keycenter=40
lovel=1
hivel=127
tune=5
transpose=-2
volume=-3.5
amp_veltrack=80
pan=-40
loop_mode=loop_sustain
loop_start=1000
loop_end=20000
offset=50
end=30000
ampeg_attack=0.005
ampeg_hold=0.1
ampeg_decay=0.5
ampeg_sustain=70
ampeg_release=0.3
group=4
off_by=3
off_mode=normal
trigger=release
seq_length=2
seq_position=0
lorand=0.2
hirand=0.8
";
    let doc = parse_text(text).expect("full subset");
    let r = &doc.regions[0];
    assert_eq!(r.lokey, Some(36));
    assert_eq!(r.hikey, Some(48));
    assert_eq!(r.pitch_keycenter, Some(40));
    assert_eq!(r.lovel, Some(1));
    assert_eq!(r.hivel, Some(127));
    assert_eq!(r.tune, Some(5));
    assert_eq!(r.transpose, Some(-2));
    assert_eq!(r.volume, Some(-3.5));
    assert_eq!(r.amp_veltrack, Some(80.0));
    assert_eq!(r.pan, Some(-40.0));
    assert_eq!(r.loop_mode, Some(LoopMode::LoopSustain));
    assert_eq!(r.loop_start, Some(1000));
    assert_eq!(r.loop_end, Some(20000));
    assert_eq!(r.offset, Some(50));
    assert_eq!(r.end, Some(30000));
    assert_eq!(r.ampeg_attack, Some(0.005));
    assert_eq!(r.ampeg_hold, Some(0.1));
    assert_eq!(r.ampeg_decay, Some(0.5));
    assert_eq!(r.ampeg_sustain, Some(70.0));
    assert_eq!(r.ampeg_release, Some(0.3));
    assert_eq!(r.group, Some(4));
    assert_eq!(r.off_by, Some(3));
    assert_eq!(r.off_mode, Some(OffMode::Normal));
    assert_eq!(r.trigger, Some(Trigger::Release));
    assert_eq!(r.seq_length, Some(2));
    assert_eq!(r.seq_position, Some(0));
    assert_eq!(r.lorand, Some(0.2));
    assert_eq!(r.hirand, Some(0.8));
    assert!(doc.warnings.is_empty());
}

/// A `<region>` with no opcodes still yields a region (with inherited values).
#[test]
fn empty_region_is_kept() {
    let doc = parse_text("<global>\nvolume=-1\n<region>\n").expect("parse");
    assert_eq!(doc.regions.len(), 1);
    assert_eq!(doc.regions[0].volume, Some(-1.0));
}

/// The `Region` type exposes all documented fields (compile-time check).
#[test]
fn region_fields_exist() {
    let r = Region::default();
    let _ = (
        r.sample,
        r.lokey,
        r.hikey,
        r.pitch_keycenter,
        r.lovel,
        r.hivel,
        r.tune,
        r.transpose,
        r.volume,
        r.amp_veltrack,
        r.pan,
        r.loop_mode,
        r.loop_start,
        r.loop_end,
        r.offset,
        r.end,
        r.ampeg_attack,
        r.ampeg_hold,
        r.ampeg_decay,
        r.ampeg_sustain,
        r.ampeg_release,
        r.group,
        r.off_by,
        r.off_mode,
        r.trigger,
        r.seq_length,
        r.seq_position,
        r.lorand,
        r.hirand,
    );
}
