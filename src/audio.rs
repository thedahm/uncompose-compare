//! Decode a source file to integer PCM, and encode that PCM as a lossless
//! playback proxy (#74).
//!
//! At load each source is transcoded into a lossless playback proxy — flacenc
//! FLAC at the source sample rate and 24-bit-max integer depth (16/24-bit
//! sources pass through bit-exact; wider integer and float sources quantize to
//! 24), with a hound WAV fallback when FLAC cannot represent the source (e.g.
//! more than eight channels). Sample rate is never touched.

use std::fs::File;
use std::path::Path;

use symphonia::core::audio::{Channels, SampleBuffer};
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// FLAC's format ceiling on channel count; a wider source falls back to WAV.
pub const MAX_FLAC_CHANNELS: usize = 8;

/// The lossless container a proxy was written in: FLAC by default, WAV only when
/// FLAC cannot represent the source (per #74).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Flac,
    Wav,
}

impl Container {
    /// The container FLAC can represent `channels` in — the whole of the
    /// fallback policy, and a pure function of the source, so the same source
    /// always maps to the same cache filename.
    pub fn for_channels(channels: u16) -> Container {
        if channels as usize > MAX_FLAC_CHANNELS {
            Container::Wav
        } else {
            Container::Flac
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Container::Flac => "flac",
            Container::Wav => "wav",
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Container::Flac => "audio/flac",
            Container::Wav => "audio/wav",
        }
    }
}

/// The proxy bit depth for a source of `src_bits`: 16- and 24-bit integer
/// sources pass through unchanged; anything wider (32-bit int, 32/64-bit float)
/// quantizes to 24 (#74). FLAC/WAV both top out at 24-bit here.
pub fn target_bits(src_bits: u32) -> u32 {
    src_bits.clamp(8, 24)
}

/// Fully-decoded source PCM plus the transcode policy derived from it: the
/// interleaved samples (right-shifted into `bits`-bit integer range), the source
/// sample rate (never resampled, #74), the channel count and layout, and the
/// target bit depth (16/24 pass through bit-exact; 32-bit int and float quantize
/// to 24).
pub struct DecodedPcm {
    /// Interleaved integer samples in `bits`-bit range, ready for the encoder.
    pub samples: Vec<i32>,
    pub sample_rate: u32,
    pub channels: u16,
    /// Which speaker position each interleaved channel holds, when the source
    /// declared one — the loudness meter needs it to weight the channels the way
    /// BS.1770 does (issue #30). `None` for a source that names no layout; the
    /// meter then falls back to ebur128's positional default.
    pub layout: Option<Channels>,
    pub bits: u32,
}

impl DecodedPcm {
    pub fn frames(&self) -> u64 {
        if self.channels == 0 {
            0
        } else {
            self.samples.len() as u64 / self.channels as u64
        }
    }
}

/// Decode `path` fully with symphonia into interleaved integer PCM. Decoding to
/// the end (rather than trusting a header frame count) is what makes an
/// undecodable or truncated file surface here as an error instead of a wrong
/// duration or a broken proxy later.
///
/// symphonia's `SampleBuffer<i32>` normalizes every source format to the full
/// i32 range (`i16 << 16`, `i24 << 8`, float scaled to fill i32, `i32` as-is),
/// so a single right shift by `32 - bits` recovers a `bits`-bit sample: exact
/// for 16- and 24-bit integer sources, a top-bits quantization for 32-bit int
/// and float. Sample rate is carried through untouched.
pub fn decode_pcm(path: &Path) -> Result<DecodedPcm, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| e.to_string())?;

    let mut format = probed.format;
    // Only the track id and codec params outlive this borrow of `format`;
    // `next_packet` below needs `format` mutable again.
    let track = format
        .default_track()
        .ok_or_else(|| "no audio track".to_string())?;
    let track_id = track.id;
    let codec_params = track.codec_params.clone();

    let sample_rate = codec_params
        .sample_rate
        .ok_or_else(|| "unknown sample rate".to_string())?;
    let channels = codec_params.channels.map(|c| c.count() as u16).unwrap_or(0);
    if channels == 0 {
        return Err("no channels".to_string());
    }

    // Target bit depth: preserve 16/24-bit integer sources bit-exact; anything
    // wider (32-bit int, 32/64-bit float) quantizes to 24 (#74). An unstated
    // depth defaults to 24, the widest we ever emit.
    let bits = target_bits(codec_params.bits_per_sample.unwrap_or(24));
    let shift = 32 - bits;

    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .map_err(|e| e.to_string())?;

    let mut samples: Vec<i32> = Vec::new();
    let mut sample_buf: Option<SampleBuffer<i32>> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            // A clean end of stream is the loop's exit, not a failure.
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break
            }
            Err(e) => return Err(e.to_string()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(buf) => buf,
            // A recoverable decode hiccup skips the packet; a fatal one fails.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(e.to_string()),
        };
        let buf = sample_buf.get_or_insert_with(|| {
            SampleBuffer::<i32>::new(decoded.capacity() as u64, *decoded.spec())
        });
        buf.copy_interleaved_ref(decoded);
        samples.extend(buf.samples().iter().map(|&s| s >> shift));
    }

    Ok(DecodedPcm {
        samples,
        sample_rate,
        channels,
        layout: codec_params.channels,
        bits,
    })
}

/// The ITU-R BS.1770 integrated loudness of the decoded PCM, in LUFS (issue
/// #30). The samples are `bits`-bit integers packed in `i32`; normalizing them
/// to full-scale floats (`s / 2^(bits-1)`) makes the measurement absolute — the
/// same signal at the same level reads the same LUFS regardless of source depth,
/// so the two candidates' figures are directly comparable.
///
/// A signal too quiet or too short for the integrated gate reads as
/// `f64::NEG_INFINITY` (ebur128's convention); the caller decides what a
/// non-finite reading means for the match.
///
/// The samples are normalized a chunk at a time rather than into one full-length
/// float copy: a long high-rate input is already the largest thing in memory at
/// load, and doubling it for the meter is what turns a load that fits into one
/// that does not.
pub fn integrated_lufs(pcm: &DecodedPcm) -> Result<f64, String> {
    let channels = pcm.channels as usize;
    if channels == 0 {
        return Err("no channels".to_string());
    }
    let mut meter = ebur128::EbuR128::new(pcm.channels as u32, pcm.sample_rate, ebur128::Mode::I)
        .map_err(|e| format!("loudness meter: {e:?}"))?;
    // Weight each channel by the position the source declared (BS.1770 counts
    // surround channels +1.5 dB and excludes LFE). ebur128's default map is
    // right for mono/stereo/quad/5.x and wrong past that — it leaves every
    // channel beyond the sixth unweighted — so a declared layout always decides.
    if let Some(layout) = pcm.layout {
        for (index, channel) in layout.iter().take(channels).enumerate() {
            meter
                .set_channel(index as u32, bs1770_position(channel))
                .map_err(|e| format!("loudness channel map: {e:?}"))?;
        }
    }
    // Full-scale is 2^(bits-1); a `bits`-bit sample divided by it lands in
    // [-1, 1], the range ebur128's float path expects.
    let scale = (1i64 << (pcm.bits.saturating_sub(1))) as f32;
    let mut scratch: Vec<f32> = Vec::with_capacity(METER_CHUNK_FRAMES * channels);
    for chunk in pcm.samples.chunks(METER_CHUNK_FRAMES * channels) {
        scratch.clear();
        scratch.extend(chunk.iter().map(|&s| s as f32 / scale));
        meter
            .add_frames_f32(&scratch)
            .map_err(|e| format!("loudness measure: {e:?}"))?;
    }
    meter
        .loudness_global()
        .map_err(|e| format!("loudness global: {e:?}"))
}

/// Frames per metering chunk: enough that the per-call overhead is noise, small
/// enough that the scratch buffer stays a rounding error next to the PCM.
const METER_CHUNK_FRAMES: usize = 16_384;

/// The BS.1770 position for one declared source channel.
///
/// The three front channels are counted at unity, the surround channels at the
/// standard +1.5 dB (ebur128 applies that weight to its surround and ±60°/±90°
/// positions), and LFE is excluded from the measurement entirely. A position
/// BS.1770 does not name — height and wide channels, which arrive only from
/// BS.2051-style layouts — is counted at unity, which is what
/// `Channel::Center` means to the meter's weight table.
fn bs1770_position(channel: Channels) -> ebur128::Channel {
    use ebur128::Channel;
    match channel {
        Channels::FRONT_LEFT => Channel::Left,
        Channels::FRONT_RIGHT => Channel::Right,
        Channels::FRONT_CENTRE => Channel::Center,
        // The one position BS.1770 explicitly leaves out of the measurement.
        Channels::LFE1 | Channels::LFE2 => Channel::Unused,
        Channels::REAR_LEFT => Channel::LeftSurround,
        Channels::REAR_RIGHT => Channel::RightSurround,
        Channels::SIDE_LEFT => Channel::Mp090,
        Channels::SIDE_RIGHT => Channel::Mm090,
        Channels::FRONT_LEFT_CENTRE => Channel::MpSC,
        Channels::FRONT_RIGHT_CENTRE => Channel::MmSC,
        Channels::REAR_CENTRE => Channel::Mp180,
        _ => Channel::Center,
    }
}

/// Encode interleaved integer PCM in `container` at the source sample rate and
/// target bit depth (#74).
pub fn encode(pcm: &DecodedPcm, container: Container) -> Result<Vec<u8>, String> {
    match container {
        Container::Flac => encode_flac(pcm),
        Container::Wav => encode_wav(pcm),
    }
}

/// Encode interleaved integer PCM as FLAC at the source sample rate and target
/// bit depth (flacenc, #74).
fn encode_flac(pcm: &DecodedPcm) -> Result<Vec<u8>, String> {
    use flacenc::component::BitRepr;
    use flacenc::error::Verify;

    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|(_, e)| format!("flac config: {e}"))?;
    let source = flacenc::source::MemSource::from_samples(
        &pcm.samples,
        pcm.channels as usize,
        pcm.bits as usize,
        pcm.sample_rate as usize,
    );
    let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|e| format!("flac encode: {e}"))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| format!("flac serialize: {e}"))?;
    Ok(sink.as_slice().to_vec())
}

/// Encode interleaved integer PCM as WAV (hound) — the fallback for sources FLAC
/// cannot represent, at the source sample rate and target bit depth (#74).
fn encode_wav(pcm: &DecodedPcm) -> Result<Vec<u8>, String> {
    let spec = hound::WavSpec {
        channels: pcm.channels,
        sample_rate: pcm.sample_rate,
        bits_per_sample: pcm.bits as u16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut writer =
            hound::WavWriter::new(&mut buf, spec).map_err(|e| format!("wav writer: {e}"))?;
        for &s in &pcm.samples {
            writer
                .write_sample(s)
                .map_err(|e| format!("wav sample: {e}"))?;
        }
        writer
            .finalize()
            .map_err(|e| format!("wav finalize: {e}"))?;
    }
    Ok(buf.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_bits_preserves_16_and_24_but_caps_wider() {
        assert_eq!(target_bits(16), 16, "16-bit passes through");
        assert_eq!(target_bits(24), 24, "24-bit passes through");
        assert_eq!(target_bits(32), 24, "32-bit int quantizes to 24");
        assert_eq!(target_bits(64), 24, "64-bit float quantizes to 24");
        assert_eq!(target_bits(8), 8, "8-bit stays 8");
    }

    /// symphonia normalizes every source to the full i32 range; the decode loop
    /// then recovers the target-bit sample with `>> (32 - bits)`. Prove that
    /// recovers 16- and 24-bit sources bit-exact and takes the top 24 bits of a
    /// 32-bit source.
    #[test]
    fn shift_recovers_source_bits() {
        // A 16-bit sample lands in i32 as `orig << 16`; >> 16 recovers it.
        let orig16: i32 = -12_345;
        assert_eq!((orig16 << 16) >> (32 - target_bits(16)), orig16);

        // A 24-bit sample lands as `orig << 8`; >> 8 recovers it.
        let orig24: i32 = 3_000_000; // within +/- 2^23
        assert_eq!((orig24 << 8) >> (32 - target_bits(24)), orig24);

        // A full-range i32 (32-bit source) keeps its top 24 bits.
        let full: i32 = 0x7FAB_CDEF;
        assert_eq!(full >> (32 - target_bits(32)), full >> 8);
    }

    /// The canonical 7.1 layout, in the bit order symphonia interleaves it:
    /// front L/R/C, LFE, rear L/R, side L/R.
    fn surround_71() -> Channels {
        Channels::FRONT_LEFT
            | Channels::FRONT_RIGHT
            | Channels::FRONT_CENTRE
            | Channels::LFE1
            | Channels::REAR_LEFT
            | Channels::REAR_RIGHT
            | Channels::SIDE_LEFT
            | Channels::SIDE_RIGHT
    }

    /// One second of 16-bit 1 kHz tone at −6 dBFS, present only in the given
    /// interleaved channel indices and silent in every other.
    fn tone_in(channels: u16, layout: Channels, present: &[usize]) -> DecodedPcm {
        let sample_rate = 48_000u32;
        let mut samples = vec![0i32; sample_rate as usize * channels as usize];
        for frame in 0..sample_rate as usize {
            let t = frame as f64 / sample_rate as f64;
            let value = ((t * 1000.0 * std::f64::consts::TAU).sin() * 16_384.0) as i32;
            for &channel in present {
                samples[frame * channels as usize + channel] = value;
            }
        }
        DecodedPcm {
            samples,
            sample_rate,
            channels,
            layout: Some(layout),
            bits: 16,
        }
    }

    /// The declared layout — not ebur128's positional default — decides which
    /// channels the measurement counts. The default map leaves everything past
    /// the sixth channel unweighted, so a 7.1 source whose only content sits in
    /// the side channels would otherwise measure as silence.
    #[test]
    fn side_channels_of_a_71_source_are_measured() {
        let pcm = tone_in(8, surround_71(), &[6, 7]);
        let lufs = integrated_lufs(&pcm).expect("the meter runs");
        assert!(
            lufs.is_finite() && lufs > -30.0,
            "the side channels carry the whole signal: {lufs} LUFS"
        );
    }

    /// LFE is excluded from BS.1770 integrated loudness, so a source whose only
    /// content is in the LFE channel has no measurable loudness.
    #[test]
    fn the_lfe_channel_is_excluded_from_the_measurement() {
        let pcm = tone_in(8, surround_71(), &[3]);
        let lufs = integrated_lufs(&pcm).expect("the meter runs");
        assert!(
            !lufs.is_finite(),
            "LFE-only content measures as no reading, not a level: {lufs} LUFS"
        );
    }

    /// Metering a chunk at a time (rather than one full-length float copy) must
    /// not move the reading: a −6 dBFS 1 kHz tone spanning several chunks lands
    /// where such a tone belongs.
    #[test]
    fn chunked_metering_measures_a_stereo_tone_at_its_level() {
        let pcm = tone_in(2, Channels::FRONT_LEFT | Channels::FRONT_RIGHT, &[0, 1]);
        assert!(
            pcm.samples.len() > 2 * METER_CHUNK_FRAMES * 2,
            "the fixture spans several metering chunks"
        );
        let lufs = integrated_lufs(&pcm).expect("the meter runs");
        // A −6 dBFS sine is −9 dBFS RMS per channel; two coherent channels sum
        // to about −6 LUFS. Assert the neighborhood, not a digit.
        assert!(
            (lufs - -6.0).abs() < 1.5,
            "a −6 dBFS 1 kHz stereo tone measures near −6 LUFS, got {lufs}"
        );
    }
}
