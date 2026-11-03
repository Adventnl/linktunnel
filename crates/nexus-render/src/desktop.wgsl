@group(0) @binding(0) var picture: texture_2d<f32>;
@group(0) @binding(1) var picture_sampler: sampler;
@group(0) @binding(2) var<uniform> crop: vec4f;
struct Vertex { @builtin(position) position: vec4f, @location(0) uv: vec2f }
@vertex fn vs(@builtin(vertex_index) i: u32) -> Vertex {
    let positions = array(vec2f(-1, 1), vec2f(-1, -1), vec2f(1, 1), vec2f(1, -1));
    let uv = array(vec2f(0, 0), vec2f(0, 1), vec2f(1, 0), vec2f(1, 1));
    return Vertex(vec4f(positions[i], 0, 1), crop.xy + uv[i] * crop.zw);
}
@fragment fn fs(v: Vertex) -> @location(0) vec4f {
    return vec4f(textureSample(picture, picture_sampler, v.uv).rgb, 1);
}
