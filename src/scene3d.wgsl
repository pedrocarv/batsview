struct Uniforms {
    view_projection: mat4x4<f32>,
    limits: vec4<f32>,
    shape: vec4<f32>,
    style: vec4<f32>,
    solid_color: vec4<f32>,
    model: vec4<f32>,
};

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var colormaps: texture_2d<f32>;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) value: f32,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) value: f32,
    @location(1) world_position: vec3<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    let world_position = input.position * uniforms.model.x;
    output.position = uniforms.view_projection * vec4<f32>(world_position, 1.0);
    output.value = input.value;
    output.world_position = world_position;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    if (uniforms.style.y < 0.5 && (input.value != input.value || abs(input.value) > 3.402823e38)) {
        discard;
    }
    var output_color = uniforms.solid_color;
    if (uniforms.style.y > 0.5) {
        output_color.a = uniforms.style.x;
    } else {
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
    output_color = textureLoad(
        colormaps,
        vec2<i32>(i32(round(normalized * 255.0)), i32(round(uniforms.shape.x))),
        0,
    );
    output_color.a = output_color.a * uniforms.style.x;
    }
    if (uniforms.style.z > 0.5) {
        let normal = normalize(cross(dpdx(input.world_position), dpdy(input.world_position)));
        let light = normalize(vec3<f32>(0.35, 0.45, 0.82));
        let diffuse = 0.68 + 0.32 * abs(dot(normal, light));
        output_color = vec4<f32>(output_color.rgb * diffuse, output_color.a);
    }
    return output_color;
}
