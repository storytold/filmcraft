//! Run with an isolated hardware factory so concurrent engine tests retain their own backends.

use filmcraft_engine::Session;
use filmcraft_render::Image;
use filmcraft_render::flow::{FlowField, NeuralSession, register_neural_enhancer};
use serde_json::json;

struct OneHardwareFrame(bool);

impl NeuralSession for OneHardwareFrame {
    fn backend(&self) -> &'static str {
        "test GPU"
    }

    fn enhance(&mut self, curr: &Image, _: &FlowField, _: bool) -> Result<Image, String> {
        if std::mem::take(&mut self.0) { Ok(curr.clone()) } else { Err("injected readback failure".into()) }
    }
}

#[test]
fn reconstruction_fallback_does_not_count_a_cpu_draw_as_gpu() {
    register_neural_enhancer(|_, _| Ok(Box::new(OneHardwareFrame(true))));
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let seq = session.active_sequence().unwrap();
    let first = &seq.video_tracks[0].items[0];
    let (id, cut) = (first.id, first.start + seq.settings.frame_rate.tick_of(3));
    session.execute("timeline.razor", json!({"clip": id.0, "time": cut.0})).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let result = session.execute("clip.opticalFlowOverlay.dlssEnhance", json!({"clips":[id.0], "dir": dir.path(), "wait":true})).unwrap();
    assert_eq!(result["reconstructionHardwareFrames"], 1);
    assert_eq!(result["reconstructionCpuFrames"], 2);
    assert!(result["reconstructionFallbackReason"].as_str().unwrap().contains("injected readback failure"));
    assert_eq!(result["overlayBackend"], "CPU");
    assert_eq!(result["overlayGpuFrames"], 0);
    assert_eq!(result["overlayCpuFrames"], 3);
    assert!(result["overlayFallbackReason"].is_null());
}
