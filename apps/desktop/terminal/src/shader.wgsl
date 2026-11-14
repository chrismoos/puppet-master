struct Viewport {
  size: vec2<f32>,
  _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> viewport: Viewport;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

fn corner(index: u32) -> vec2<f32> {
  return vec2<f32>(f32(index & 1u), f32((index >> 1u) & 1u));
}

fn to_clip(pixel: vec2<f32>) -> vec4<f32> {
  let ndc = pixel / viewport.size * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0);
  return vec4<f32>(ndc, 0.0, 1.0);
}

struct RectOut {
  @builtin(position) position: vec4<f32>,
  @location(0) color: vec4<f32>,
};

@vertex
fn rect_vs(
  @builtin(vertex_index) vertex: u32,
  @location(0) origin: vec2<f32>,
  @location(1) size: vec2<f32>,
  @location(2) color: vec4<f32>,
) -> RectOut {
  var out: RectOut;
  out.position = to_clip(origin + corner(vertex) * size);
  out.color = color;
  return out;
}

@fragment
fn rect_fs(in: RectOut) -> @location(0) vec4<f32> {
  return in.color;
}

struct GlyphOut {
  @builtin(position) position: vec4<f32>,
  @location(0) uv: vec2<f32>,
  @location(1) color: vec4<f32>,
};

@vertex
fn glyph_vs(
  @builtin(vertex_index) vertex: u32,
  @location(0) origin: vec2<f32>,
  @location(1) size: vec2<f32>,
  @location(2) uv_origin: vec2<f32>,
  @location(3) uv_size: vec2<f32>,
  @location(4) color: vec4<f32>,
) -> GlyphOut {
  var out: GlyphOut;
  let c = corner(vertex);
  out.position = to_clip(origin + c * size);
  out.uv = uv_origin + c * uv_size;
  out.color = color;
  return out;
}

@fragment
fn glyph_fs(in: GlyphOut) -> @location(0) vec4<f32> {
  let coverage = textureSample(atlas, atlas_sampler, in.uv).r;
  return vec4<f32>(in.color.rgb, in.color.a * coverage);
}
