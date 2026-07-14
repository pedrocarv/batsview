struct Uniforms {
    bounds: vec4<f32>,
    limits: vec4<f32>,
    shape: vec4<f32>,
};

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var colormaps: texture_2d<f32>;

struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) value: f32,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) value: f32,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    let center = vec2<f32>(
        0.5 * (uniforms.bounds.x + uniforms.bounds.y),
        0.5 * (uniforms.bounds.z + uniforms.bounds.w),
    );
    let span = vec2<f32>(
        max(uniforms.bounds.y - uniforms.bounds.x, 1e-20),
        max(uniforms.bounds.w - uniforms.bounds.z, 1e-20),
    );
    var position = 2.0 * (input.position - center) / span;
    var output: VertexOutput;
    output.position = vec4<f32>(position.x, position.y, 0.0, 1.0);
    output.value = input.value;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    if (input.value != input.value || abs(input.value) > 3.402823e38) {
        discard;
    }
    var value = input.value;
    var low = uniforms.limits.x;
    var high = uniforms.limits.y;
    if (uniforms.shape.y > 0.5) {
        if (value <= 0.0) {
            discard;
        }
        value = log(value) / log(10.0);
        low = log(max(low, uniforms.limits.z)) / log(10.0);
        high = log(max(high, uniforms.limits.z)) / log(10.0);
    }
    var normalized = clamp((value - low) / max(high - low, 1e-20), 0.0, 1.0);
    if (uniforms.shape.z > 0.5) {
        normalized = 1.0 - normalized;
    }
    let bins = i32(round(uniforms.shape.w));
    if (bins >= 2) {
        let index = min(i32(floor(normalized * f32(bins))), bins - 1);
        normalized = f32(index) / f32(bins - 1);
    }
    let x = i32(round(normalized * 255.0));
    let y = i32(round(uniforms.shape.x));
    return textureLoad(colormaps, vec2<i32>(x, y), 0);
}
