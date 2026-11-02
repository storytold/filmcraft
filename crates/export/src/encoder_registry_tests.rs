use super::{
    EncodedPacket, EncoderFactory, EncoderFrame, ExportSettings, Format, FrameRate, Result, SampleEntry, VideoEncoder, register_encoder, video_factories,
};

/// Encoder that reports its factory through its time scale, so the test can tell who answered.
struct Tagged(u32);

impl VideoEncoder for Tagged {
    fn sample_entry(&self) -> SampleEntry {
        SampleEntry::jpeg(1, 1)
    }
    fn timescale(&self) -> u32 {
        self.0
    }
    fn encode(&mut self, _: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
        Ok(Vec::new())
    }
    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        Ok(Vec::new())
    }
}

// Answer only H.264 at 7x9, a size no other test uses: the process-wide registry stays inert elsewhere.
fn first_fake(format: Format, w: u32, h: u32, _: FrameRate, _: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    (format == Format::H264 && (w, h) == (7, 9)).then(|| Ok(Box::new(Tagged(101)) as Box<dyn VideoEncoder>))
}

fn second_fake(format: Format, w: u32, h: u32, _: FrameRate, _: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    (format == Format::H264 && (w, h) == (7, 9)).then(|| Ok(Box::new(Tagged(202)) as Box<dyn VideoEncoder>))
}

#[test]
fn register_encoder_puts_latest_first_and_is_idempotent() {
    let first: EncoderFactory = first_fake;
    let second: EncoderFactory = second_fake;
    register_encoder(first);
    register_encoder(second);
    register_encoder(first);
    let factories: Vec<EncoderFactory> = video_factories().read().unwrap_or_else(|e| e.into_inner()).clone();
    let count = |f: EncoderFactory| factories.iter().filter(|x| std::ptr::fn_addr_eq(**x, f)).count();
    assert_eq!(count(first), 1, "registering the same factory again must not duplicate it");
    assert_eq!(count(second), 1);
    let position = |f: EncoderFactory| factories.iter().position(|x| std::ptr::fn_addr_eq(*x, f)).expect("registered");
    assert!(position(second) < position(first), "the latest registration is tried first");
    let winner = factories.iter().find_map(|f| f(Format::H264, 7, 9, FrameRate::FPS_24, &ExportSettings::default()));
    let encoder = winner.expect("a factory answers at 7x9").expect("the factory builds an encoder");
    assert_eq!(encoder.timescale(), 202, "the latest registered factory answers first");
}
