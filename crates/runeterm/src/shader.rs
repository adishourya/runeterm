//! Ghostty-compatible custom shaders (`custom-shader = …`): Shadertoy-style GLSL whose
//! `mainImage` reads the terminal from `iChannel0` and writes the pixels shown. Each pane renders
//! its grid offscreen, runs the chain over it, and the result is composited into the pane's
//! rounded surface. The uniforms are Ghostty's, cursor ones included, so shaders like
//! `cursor_glide.glsl` work unchanged.

use bytemuck::{Pod, Zeroable};
use std::path::Path;
use wgpu::naga;

/// Ghostty's shader prelude, adapted to wgpu: the uniforms live in one std140 block of vec4s
/// (named back by `#define`s), and `iChannel0` is a texture with its sampler.
const PRELUDE: &str = r#"#version 450
layout(set = 0, binding = 0, std140) uniform Globals {
    vec4 _res;      // xyz: iResolution
    vec4 _time;     // iTime, iTimeDelta, iFrameRate, iTimeCursorChange
    ivec4 _ints;    // iFrame, iCurrentCursorStyle, iPreviousCursorStyle, iCursorVisible
    vec4 _focus;    // iTimeFocus, iFocus
    vec4 iMouse;
    vec4 iDate;
    vec4 iCurrentCursor;
    vec4 iPreviousCursor;
    vec4 iCurrentCursorColor;
    vec4 iPreviousCursorColor;
    vec4 _bg;
    vec4 _fg;
    vec4 _cursor;
    vec4 _cursor_text;
    vec4 _sel_fg;
    vec4 _sel_bg;
};
layout(set = 0, binding = 1) uniform texture2D _channel0;
layout(set = 0, binding = 2) uniform sampler _channel0_sampler;
#define iChannel0 sampler2D(_channel0, _channel0_sampler)
#define iResolution (_res.xyz)
#define iTime (_time.x)
#define iTimeDelta (_time.y)
#define iFrameRate (_time.z)
#define iTimeCursorChange (_time.w)
#define iFrame (_ints.x)
#define iCurrentCursorStyle (_ints.y)
#define iPreviousCursorStyle (_ints.z)
#define iCursorVisible (_ints.w)
#define iTimeFocus (_focus.x)
#define iFocus (int(_focus.y))
#define iSampleRate 44100.0
#define iBackgroundColor (_bg.rgb)
#define iForegroundColor (_fg.rgb)
#define iCursorColor (_cursor.rgb)
#define iCursorText (_cursor_text.rgb)
#define iSelectionForegroundColor (_sel_fg.rgb)
#define iSelectionBackgroundColor (_sel_bg.rgb)
#define CURSORSTYLE_BLOCK 0
#define CURSORSTYLE_BLOCK_HOLLOW 1
#define CURSORSTYLE_BAR 2
#define CURSORSTYLE_UNDERLINE 3
#define CURSORSTYLE_LOCK 4
#define texture2D texture
layout(location = 0) out vec4 _fragColor;
"#;

/// After the user's code: naga wants functions defined before they're called.
const POSTLUDE: &str = r#"
void main() {
    // Shadertoy's y points up.
    mainImage(_fragColor, vec2(gl_FragCoord.x, _res.y - gl_FragCoord.y));
}
"#;

const VERTEX: &str = r#"
@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}
"#;

/// The uniforms, laid out as `Globals` above.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Default)]
pub struct Globals {
    pub res: [f32; 4],
    pub time: [f32; 4],
    pub ints: [i32; 4],
    pub focus: [f32; 4],
    pub mouse: [f32; 4],
    pub date: [f32; 4],
    pub cursor: [f32; 4],
    pub prev_cursor: [f32; 4],
    pub cursor_color: [f32; 4],
    pub prev_cursor_color: [f32; 4],
    pub bg: [f32; 4],
    pub fg: [f32; 4],
    pub cursor_rgb: [f32; 4],
    pub cursor_text: [f32; 4],
    pub sel_fg: [f32; 4],
    pub sel_bg: [f32; 4],
}

/// Ghostty's cursor style numbers.
pub fn style_number(s: crate::vt::CursorStyle) -> i32 {
    use crate::vt::CursorStyle::*;
    match s {
        Block => 0,
        BlockHollow => 1,
        Bar => 2,
        Underline => 3,
    }
}

/// Reads and checks a shader. A shader that doesn't compile is reported and skipped, as Ghostty
/// does, rather than taking the terminal down.
pub fn load(path: &Path) -> Option<String> {
    let user = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("runeterm: custom-shader {}: {e}", path.display());
            return None;
        }
    };
    let source = format!("{PRELUDE}\n#line 1\n{user}\n{POSTLUDE}");
    let mut frontend = naga::front::glsl::Frontend::default();
    let module = match frontend.parse(&naga::front::glsl::Options::from(naga::ShaderStage::Fragment), &source) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("runeterm: custom-shader {}: {}", path.display(), e.emit_to_string(&source));
            return None;
        }
    };
    let mut v = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all());
    if let Err(e) = v.validate(&module) {
        eprintln!("runeterm: custom-shader {}: {}", path.display(), e.emit_to_string(&source));
        return None;
    }
    Some(source)
}

/// The pipelines of a shader chain, built for one device.
pub struct Chain {
    pub pipelines: Vec<wgpu::RenderPipeline>,
    pub layout: wgpu::BindGroupLayout,
    pub sampler: wgpu::Sampler,
}

pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

impl Chain {
    pub fn new(device: &wgpu::Device, sources: &[String]) -> Chain {
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::FRAGMENT, ty, count: None };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("runeterm shader"),
            entries: &[
                entry(0, wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }),
                entry(1, wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                }),
                entry(2, wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)),
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("runeterm shader"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let vs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("runeterm shader vs"),
            source: wgpu::ShaderSource::Wgsl(VERTEX.into()),
        });
        let pipelines = sources
            .iter()
            .map(|src| {
                let fs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("runeterm custom shader"),
                    source: wgpu::ShaderSource::Glsl { shader: src.as_str().into(), stage: naga::ShaderStage::Fragment, defines: &[] },
                });
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("runeterm custom shader"),
                    layout: Some(&pl),
                    vertex: wgpu::VertexState { module: &vs, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
                    primitive: Default::default(),
                    depth_stencil: None,
                    multisample: Default::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &fs,
                        entry_point: Some("main"),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState { format: FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            })
            .collect();
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Chain { pipelines, layout, sampler }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(body: &str) -> Option<String> {
        let dir = std::env::temp_dir().join(format!("runeterm-shader-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(format!("{}.glsl", body.len()));
        std::fs::write(&p, body).unwrap();
        load(&p)
    }

    #[test]
    fn accepts_ghostty_shaders_and_rejects_broken_ones() {
        let ok = "void mainImage(out vec4 c, in vec2 p) { vec4 t = texture(iChannel0, p / iResolution.xy); \
                  if (iCursorVisible == 1 && iCurrentCursorStyle == CURSORSTYLE_BAR) t.r += iTime * 0.0; c = t + iCurrentCursor * 0.0; }";
        assert!(check(ok).is_some());
        assert!(check("void mainImage(out vec4 c, in vec2 p) { c = nope; }").is_none());
    }

    #[test]
    fn the_users_shaders_compile() {
        let dir = std::path::PathBuf::from(std::env::var("HOME").unwrap()).join(".config/ghostty/shaders");
        let Ok(entries) = std::fs::read_dir(&dir) else { return };
        for e in entries.flatten() {
            if e.path().extension().is_some_and(|x| x == "glsl") {
                assert!(load(&e.path()).is_some(), "{} should compile", e.path().display());
            }
        }
    }
}
