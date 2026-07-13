struct Uniforms {
    bounds: vec4<f32>,
    limits: vec4<f32>,
    view: vec4<f32>,
    shape: vec4<f32>,
};

@group(0) @binding(0) var<uniform> uniforms: Uniforms;

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
    let data_aspect = uniforms.shape.x;
    let viewport_aspect = max(uniforms.view.w, 1e-6);
    if (data_aspect > viewport_aspect) {
        position.y *= viewport_aspect / data_aspect;
    } else {
        position.x *= data_aspect / viewport_aspect;
    }
    position = position * uniforms.view.z + uniforms.view.xy;
    var output: VertexOutput;
    output.position = vec4<f32>(position.x, position.y, 0.0, 1.0);
    output.value = input.value;
    return output;
}

fn turbo(x: f32) -> vec3<f32> {
    let k_red = vec4<f32>(0.13572138, 4.61539260, -42.66032258, 132.13108234);
    let k_green = vec4<f32>(0.09140261, 2.19418839, 4.84296658, -14.18503333);
    let k_blue = vec4<f32>(0.10667330, 12.64194608, -60.58204836, 110.36276771);
    let k2 = vec2<f32>(-152.94239396, 59.28637943);
    let k2g = vec2<f32>(4.27729857, 2.82956604);
    let k2b = vec2<f32>(-89.90310912, 27.34824973);
    let v4 = vec4<f32>(1.0, x, x * x, x * x * x);
    let v2 = v4.zw * v4.z;
    return clamp(vec3<f32>(
        dot(v4, k_red) + dot(v2, k2),
        dot(v4, k_green) + dot(v2, k2g),
        dot(v4, k_blue) + dot(v2, k2b),
    ), vec3<f32>(0.0), vec3<f32>(1.0));
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
    let normalized = clamp((value - low) / max(high - low, 1e-20), 0.0, 1.0);
    return vec4<f32>(turbo(normalized), 1.0);
}
