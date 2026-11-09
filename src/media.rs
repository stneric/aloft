use crate::gpu::Gpu;
use gst::prelude::*;

pub fn init(render: Option<&Gpu>) -> Result<(), String> {
    gst::init().map_err(|e| e.to_string())?;
    gstgtk4::plugin_register_static().map_err(|e| e.to_string())?;
    rank_decoders(render);
    Ok(())
}

fn va_device(f: &gst::ElementFactory) -> Option<String> {
    let el = f.create().build().ok()?;
    el.find_property("device-path")?;
    el.property::<Option<String>>("device-path")
}

fn rank_decoders(render: Option<&Gpu>) {
    let Some(gpu) = render else {
        return;
    };
    let node = gpu.node.to_string_lossy().into_owned();
    let mut matched = false;
    for f in gst::ElementFactory::factories_with_type(
        gst::ElementFactoryType::DECODER | gst::ElementFactoryType::MEDIA_VIDEO,
        gst::Rank::NONE,
    ) {
        let plugin = f.plugin_name().map(|s| s.to_string()).unwrap_or_default();
        match plugin.as_str() {
            "va" => match va_device(&f) {
                Some(dev) if dev == node => {
                    f.set_rank(gst::Rank::PRIMARY + 100);
                    matched = true;
                }
                Some(_) => f.set_rank(gst::Rank::NONE),
                None => {}
            },
            "nvcodec" if gpu.vendor != 0x10de => f.set_rank(gst::Rank::NONE),
            "vulkan" | "qsv" | "msdk" | "vaapi" => f.set_rank(gst::Rank::NONE),
            _ => {}
        }
    }
    if !matched {
        eprintln!(
            "aloft: no VA-API decoder for {}, falling back to software decoding",
            gpu.node.display()
        );
    }
}

pub fn va_factory(suffix: &str, node: Option<&str>) -> Option<String> {
    let mut candidates: Vec<gst::ElementFactory> = gst::Registry::get()
        .features_by_plugin("va")
        .into_iter()
        .filter_map(|f| f.downcast::<gst::ElementFactory>().ok())
        .filter(|f| {
            let n = f.name();
            n.starts_with("va") && n.ends_with(suffix)
        })
        .collect();
    candidates.sort_by_key(|f| f.name().len());
    candidates
        .into_iter()
        .find(|f| node.is_none() || va_device(f).as_deref() == node)
        .map(|f| f.name().to_string())
}

pub fn debug() -> bool {
    std::env::var_os("ALOFT_DEBUG").is_some()
}

fn fourcc(s: &str) -> Option<u32> {
    let b = s.as_bytes();
    (b.len() == 4).then(|| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Same GPU: tiled formats, RGB first (tiled NV12/P010 often ends up as a CPU copy).
/// Different GPUs: only linear buffers can be shared, and VA-API only produces
/// linear YU12, which NVIDIA's GL imports directly.
pub fn postproc_caps(postproc: &str) -> Option<gst::Caps> {
    use gtk::prelude::DisplayExt;
    const PREFERRED: [&str; 9] = [
        "AR30", "AB30", "XR30", "XB30", "AR24", "AB24", "XR24", "XB24", "NV12",
    ];

    let display = gtk::gdk::Display::default()?;
    let gdk_formats = display.dmabuf_formats();
    let gdk: std::collections::HashSet<(u32, u64)> = (0..gdk_formats.n_formats())
        .map(|i| gdk_formats.format(i))
        .collect();

    let factory = gst::ElementFactory::find(postproc)?;
    let mut offered: Vec<(String, u32, u64)> = Vec::new();
    for tmpl in factory.static_pad_templates() {
        if tmpl.direction() != gst::PadDirection::Src {
            continue;
        }
        let caps = tmpl.caps();
        for (s, features) in caps.iter_with_features() {
            if !features.contains("memory:DMABuf") {
                continue;
            }
            let Ok(list) = s.get::<gst::List>("drm-format") else {
                continue;
            };
            for v in list.iter() {
                let Ok(name) = v.get::<String>() else {
                    continue;
                };
                let (cc, modifier) = match name.split_once(':') {
                    Some((cc, m)) => (
                        cc,
                        u64::from_str_radix(m.trim_start_matches("0x"), 16).unwrap_or(u64::MAX),
                    ),
                    None => (name.as_str(), 0),
                };
                if let Some(code) = fourcc(cc) {
                    offered.push((name.clone(), code, modifier));
                }
            }
        }
    }

    let same_gpu = offered
        .iter()
        .any(|(_, c, m)| *m != 0 && gdk.contains(&(*c, *m)));
    const CROSS_GPU: [&str; 1] = ["YU12"];
    let mut candidates: Vec<(String, u32)> = Vec::new();
    if same_gpu {
        for want in PREFERRED {
            let code = fourcc(want)?;
            for (name, c, m) in &offered {
                if *c == code && gdk.contains(&(*c, *m)) {
                    candidates.push((name.clone(), code));
                }
            }
        }
    } else {
        for want in CROSS_GPU {
            let code = fourcc(want)?;
            let linear_offered = offered.iter().any(|(_, c, m)| *c == code && *m == 0);
            if linear_offered && gdk.contains(&(code, 0)) {
                candidates.push((want.to_string(), code));
            }
        }
    }
    let mut caps = gst::Caps::new_empty();
    for (name, _) in &candidates {
        let s = gst::Structure::builder("video/x-raw")
            .field("format", "DMA_DRM")
            .field("drm-format", name.as_str())
            .build();
        caps.get_mut()
            .unwrap()
            .append_structure_full(s, Some(gst::CapsFeatures::new(["memory:DMABuf"])));
    }
    if caps.is_empty() {
        return None;
    }
    // System memory as the last resort. Not ANY: that lets vapostproc pass the
    // decoder's tiled format through.
    caps.get_mut()
        .unwrap()
        .append(gst::Caps::builder("video/x-raw").build());
    if debug() {
        eprintln!("aloft: {postproc} caps: {caps}");
    }
    Some(caps)
}

pub fn video_sink(render: Option<&Gpu>) -> Result<(gst::Element, gst::Element), String> {
    let sink = gst::ElementFactory::make("gtk4paintablesink")
        .build()
        .map_err(|e| format!("gtk4paintablesink: {e}"))?;
    let node = render.map(|g| g.node.to_string_lossy().into_owned());
    let va_decoding = render.is_none_or(|g| g.vendor != 0x10de);
    let pp_name = va_factory("postproc", node.as_deref()).filter(|_| va_decoding);
    let Some(pp_name) = pp_name else {
        // nvdec: without glsinkbin the conversion happens on the CPU.
        if let Ok(glbin) = gst::ElementFactory::make("glsinkbin")
            .property("sink", &sink)
            .build()
        {
            if debug() {
                eprintln!("aloft: using glsinkbin");
            }
            return Ok((glbin, sink));
        }
        if debug() {
            eprintln!("aloft: no VA post-processor, plain sink");
        }
        return Ok((sink.clone(), sink));
    };
    let Some(caps) = postproc_caps(&pp_name) else {
        if debug() {
            eprintln!("aloft: no common dmabuf format for {pp_name}, plain sink");
        }
        return Ok((sink.clone(), sink));
    };
    let bin = gst::Bin::with_name("aloft-video-sink");
    let pp = gst::ElementFactory::make(&pp_name)
        .build()
        .map_err(|e| e.to_string())?;
    let filter = gst::ElementFactory::make("capsfilter")
        .property("caps", &caps)
        .build()
        .map_err(|e| e.to_string())?;
    bin.add_many([&pp, &filter, &sink])
        .map_err(|e| e.to_string())?;
    gst::Element::link_many([&pp, &filter, &sink]).map_err(|e| e.to_string())?;
    let pad = pp.static_pad("sink").ok_or("vapostproc without sink pad")?;
    let ghost = gst::GhostPad::with_target(&pad).map_err(|e| e.to_string())?;
    bin.add_pad(&ghost).map_err(|e| e.to_string())?;
    Ok((bin.upcast(), sink))
}
