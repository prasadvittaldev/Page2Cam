// BGRA -> NV12, BT.709 limited range. Mirrors p2c_core::bgra_to_nv12 bit for
// bit when no scaling is involved: same 16.16 integer coefficients, same
// rounding, UV = average of the four per-pixel U/V values of each 2x2 block.
// Scaling is the sampler's bilinear filter at destination pixel centres
// (the CPU reference samples at the same positions).

Texture2D<float4> src : register(t0);
SamplerState lin : register(s0);

cbuffer Dst : register(b0) {
    float2 inv_size; // 1 / (Y plane width, height)
    float2 pad;
};

struct VsOut {
    float4 pos : SV_Position;
};

// One triangle that covers the whole viewport.
VsOut vs_fullscreen(uint id : SV_VertexID) {
    float2 t = float2((id << 1) & 2, id & 2);
    VsOut o;
    o.pos = float4(t * float2(2.0, -2.0) + float2(-1.0, 1.0), 0.0, 1.0);
    return o;
}

// Source colour at Y-plane pixel centre `p` (in pixels), as 0..255 integers (r, g, b).
int3 rgb_at(float2 p) {
    float3 c = src.SampleLevel(lin, p * inv_size, 0).rgb;
    return int3(round(saturate(c) * 255.0));
}

int y_of(int3 c) {
    return 16 + ((11966 * c.r + 40255 * c.g + 4064 * c.b + 32768) >> 16);
}

int u_of(int3 c) { return -6596 * c.r - 22188 * c.g + 28784 * c.b; }
int v_of(int3 c) { return 28784 * c.r - 26145 * c.g - 2639 * c.b; }

float ps_y(VsOut i) : SV_Target {
    return y_of(rgb_at(i.pos.xy)) / 255.0;
}

float2 ps_uv(VsOut i) : SV_Target {
    // i.pos.xy = (ux + 0.5, uy + 0.5); the block's Y pixel centres are at
    // 2*pos - 0.5 and 2*pos + 0.5.
    float2 b = i.pos.xy * 2.0;
    int3 c0 = rgb_at(b + float2(-0.5, -0.5));
    int3 c1 = rgb_at(b + float2(0.5, -0.5));
    int3 c2 = rgb_at(b + float2(-0.5, 0.5));
    int3 c3 = rgb_at(b + float2(0.5, 0.5));
    int su = u_of(c0) + u_of(c1) + u_of(c2) + u_of(c3);
    int sv = v_of(c0) + v_of(c1) + v_of(c2) + v_of(c3);
    int u = clamp(128 + ((su + (1 << 17)) >> 18), 16, 240);
    int v = clamp(128 + ((sv + (1 << 17)) >> 18), 16, 240);
    return float2(u, v) / 255.0;
}
