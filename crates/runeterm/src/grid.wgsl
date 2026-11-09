// The terminal grid, drawn into a pane's effect slot: one instanced quad per background run,
// glyph and cursor. Rune's CLIP_WGSL is prepended, so the grid is cut by the same rounded
// corners as the pane surface around it.

struct U {
    // xy: the slot's top-left in this pass's pixels; zw: the pass's top-left in window pixels.
    origin: vec4<f32>,
    // xy: the pass target size in pixels.
    target_size: vec4<f32>,
    // The slot's clip rectangle (window pixels) and fade words, as Rune recorded them.
    clip: vec4<f32>,
    fade: vec4<u32>,
    // x: 1 when the frame blends in sRGB (write encoded colors), 0 for linear.
    flags: vec4<u32>,
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var color_atlas: texture_2d<f32>;
@group(0) @binding(3) var color_sampler: sampler;
@group(1) @binding(0) var image: texture_2d<f32>;
@group(1) @binding(1) var image_sampler: sampler;

struct In {
    @builtin(vertex_index) vi: u32,
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec4<f32>,
    @location(4) kind: u32,
    @location(5) pad: u32,
    @location(6) uv_size: vec2<f32>,
};

struct Out {
    @builtin(position) p: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) kind: u32,
};

@vertex
fn vs(i: In) -> Out {
    var corners = array<vec2<f32>, 6>(
        vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0),
        vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0, 1.0),
    );
    let c = corners[i.vi];
    let px = u.origin.xy + i.pos + c * i.size;
    var o: Out;
    var y = 1.0 - px.y / u.target_size.y * 2.0;
    if (u.flags.y == 1u) {
        y = -y; // offscreen for a shader: upside down, as Shadertoy's y points up
    }
    o.p = vec4(px.x / u.target_size.x * 2.0 - 1.0, y, 0.0, 1.0);
    // Glyphs address the atlas in texels; images use normalized coordinates.
    o.uv = select(i.uv + c * i.size, i.uv + c * i.uv_size, i.kind >= 2u);
    o.color = i.color;
    o.kind = i.kind;
    return o;
}

fn encode(c: vec3<f32>) -> vec3<f32> {
    return select(1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055, c * 12.92, c <= vec3(0.0031308));
}

fn linear(c: vec3<f32>) -> vec3<f32> {
    return select(pow((c + 0.055) / 1.055, vec3(2.4)), c / 12.92, c <= vec3(0.04045));
}

@fragment
fn fs(o: Out) -> @location(0) vec4<f32> {
    let px = textureSampleLevel(image, image_sampler, o.uv, 0.0);
    let emoji = textureSampleLevel(color_atlas, color_sampler, o.uv, 0.0);
    var a = o.color.a;
    var rgb = o.color.rgb;
    if (o.kind == 1u) {
        a *= textureLoad(atlas, vec2<i32>(floor(o.uv)), 0).r;
    } else if (o.kind == 2u) {
        rgb = px.rgb;
        a = px.a;
    } else if (o.kind == 4u) {
        // Colour glyph: swash gives straight (unpremultiplied) sRGB.
        rgb = emoji.rgb;
        a = emoji.a;
    } else if (o.kind == 3u) {
        // A shader chain's result: linear, premultiplied.
        let c = max(px, vec4(0.0));
        let ca = clamp(c.a, 0.0, 1.0);
        var lin = c.rgb / max(ca, 1e-5);
        if (ca <= 0.0) {
            lin = vec3(0.0);
        }
        let k = ca * clip_alpha(o.p.xy + u.origin.zw, u.clip, u.fade);
        if (u.flags.x == 1u) {
            lin = encode(clamp(lin, vec3(0.0), vec3(1.0)));
        }
        return vec4(lin * k, k);
    }
    a *= clip_alpha(o.p.xy + u.origin.zw, u.clip, u.fade);
    if (u.flags.x == 0u) {
        rgb = linear(rgb);
    }
    return vec4(rgb * a, a);
}
