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

use symphonia::core::audio::SampleBuffer;
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
/// sample rate (never resampled, #74), the channel count, and the target bit
/// depth (16/24 pass through bit-exact; 32-bit int and float quantize to 24).
pub struct DecodedPcm {
    /// Interleaved integer samples in `bits`-bit range, ready for the encoder.
    pub samples: Vec<i32>,
    pub sample_rate: u32,
    pub channels: u16,
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
        bits,
    })
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
}
