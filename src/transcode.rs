use crate::gpu::Gpu;
use crate::media::va_factory;
use gst::prelude::*;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const WIDTH: u32 = 2560;
const HEIGHT: u32 = 1440;
/// Apple's 4K files average ~9.4 Mbit/s, their 1080p ones ~4.
const BITRATE_KBPS: u32 = 7000;

fn quote(p: &Path) -> String {
    let s = p
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!("\"{s}\"")
}

pub fn pipeline_description(
    src: &Path,
    dst: &Path,
    render: Option<&Gpu>,
) -> Result<String, String> {
    let node = render.map(|g| g.node.to_string_lossy().into_owned());
    let node = node.as_deref();
    let pp = va_factory("postproc", node);
    let enc = va_factory("h265enc", node).or_else(|| va_factory("h265lpenc", node));
    let head = format!(
        "filesrc location={} ! qtdemux name=d d.video_0 ! queue max-size-time=2000000000 ! decodebin3",
        quote(src)
    );
    let tail = format!("h265parse ! mp4mux ! filesink location={}", quote(dst));
    // target-usage=7: about 2x real time on a UHD 750, the default is ~1x.
    if let (Some(pp), Some(enc)) = (pp, enc) {
        return Ok(format!(
            "{head} ! {pp} ! video/x-raw(memory:VAMemory),width={WIDTH},height={HEIGHT},format=P010_10LE \
             ! {enc} rate-control=vbr bitrate={BITRATE_KBPS} target-usage=7 ! video/x-h265,profile=main-10 ! {tail}"
        ));
    }
    if gst::ElementFactory::find("x265enc").is_some() {
        return Ok(format!(
            "{head} ! videoconvert ! videoscale ! video/x-raw,width={WIDTH},height={HEIGHT},format=I420_10LE \
             ! x265enc speed-preset=faster bitrate={BITRATE_KBPS} ! {tail}"
        ));
    }
    Err(crate::i18n::t(
        "No HEVC encoder available (VA-API or x265) to make 1440p videos",
        "Kein HEVC-Encoder (VA-API oder x265) vorhanden, um 1440p-Videos zu erzeugen",
    )
    .into())
}

pub fn can_transcode(render: Option<&Gpu>) -> bool {
    pipeline_description(Path::new("a"), Path::new("b"), render).is_ok()
}

pub fn run(
    src: &Path,
    dst: &Path,
    render: Option<&Gpu>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let desc = pipeline_description(src, dst, render)?;
    let pipeline = gst::parse::launch(&desc).map_err(|e| e.to_string())?;
    let bus = pipeline.bus().ok_or("no bus")?;
    pipeline
        .set_state(gst::State::Playing)
        .map_err(|_| "transcoding pipeline failed to start".to_string())?;
    let result = loop {
        if cancel.load(Ordering::Relaxed) {
            break Err("cancelled".to_string());
        }
        let Some(msg) = bus.timed_pop_filtered(
            gst::ClockTime::from_mseconds(250),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        ) else {
            continue;
        };
        match msg.view() {
            gst::MessageView::Eos(_) => break Ok(()),
            gst::MessageView::Error(e) => break Err(e.error().to_string()),
            _ => {}
        }
    };
    let _ = pipeline.set_state(gst::State::Null);
    std::thread::sleep(Duration::from_millis(50));
    result
}
