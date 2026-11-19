// Refined edges: the fast guided filter of p2c_compositor::GuidedScratch on
// the GPU. The model-size mask is upsampled to the refined matte size
// (g_size: the PIP size, capped to 960 wide) with the webcam luma as guide;
// the statistics run at 1/4 size (low_size) with box means of radius
// g_radius there. The matte (R8) then goes through ps_compose as the mask,
// without the edge LUT.
//
// Compiled after compose.hlsl: uses src (t0), mask (t1), lin (s0),
// PassConsts (b0), resample_rgba(), round_up4() and round_up().

Texture2D<float4> aux : register(t2);

cbuffer GuidedConsts : register(b1) {
    float2 g_size;     // refined matte size (gw, gh)
    float2 low_size;   // statistics size (ceil(gw / 4), ceil(gh / 4))
    float2 gmask_size; // model mask size
    uint g_radius;     // box radius at low_size
    float g_eps;       // regularisation (guide and mask in 0..1)
};

// The webcam scaled to g_size (as ps_compose scales it), Rec.601 luma as in
// guided.rs: (29 B + 150 G + 77 R) >> 8.
float4 ps_guide_luma(VsOut i) : SV_Target {
    float2 p = i.pos.xy - rect.xy;
    float4 c;
    [branch] if (cam_linear != 0) {
        c = round_up4(src.SampleLevel(lin, p / rect.zw, 0) * 255.0);
    } else {
        c = resample_rgba(p, src_size, rect.zw);
    }
    uint3 u = uint3(c.rgb);
    uint l = (29 * u.b + 150 * u.g + 77 * u.r) >> 8;
    return float4(float(l) / 255.0, 0.0, 0.0, 1.0);
}

float mask_at(int x, int y) {
    return round_up(mask.Load(int3(x, y, 0)) * 255.0);
}

// Per 4x4 block of the luma (src): mean guide I, mask p at the block centre
// (bilinear), I * p, I * I.
float4 ps_guide_stats(VsOut i) : SV_Target {
    int2 lp = int2(i.pos.xy);
    int2 gs = int2(g_size);
    int2 b0 = lp * 4;
    int2 b1 = min(b0 + 4, gs);
    uint sum = 0;
    [loop] for (int y = b0.y; y < b1.y; y++) {
        [loop] for (int x = b0.x; x < b1.x; x++) {
            sum += uint(round_up(src.Load(int3(x, y, 0)).r * 255.0));
        }
    }
    float n = float((b1.y - b0.y) * (b1.x - b0.x));
    float gi = float(sum) / (n * 255.0);
    float2 centre = float2(b0 + b1) * 0.5;
    float2 mc = clamp(centre * gmask_size / g_size - 0.5, 0.0, gmask_size - 1.0);
    int2 m0 = int2(floor(mc));
    int2 m1 = min(m0 + 1, int2(gmask_size) - 1);
    float2 f = mc - float2(m0);
    float m00 = mask_at(m0.x, m0.y);
    float m10 = mask_at(m1.x, m0.y);
    float m01 = mask_at(m0.x, m1.y);
    float m11 = mask_at(m1.x, m1.y);
    float top = m00 + (m10 - m00) * f.x;
    float bot = m01 + (m11 - m01) * f.x;
    float gp = (top + (bot - top) * f.y) * (1.0 / 255.0);
    return float4(gi, gp, gi * gp, gi * gi);
}

// Mean over the row window (clamped at the borders, divided by the real count).
float4 ps_box_h(VsOut i) : SV_Target {
    int2 p = int2(i.pos.xy);
    int r = int(g_radius);
    int lo = max(p.x - r, 0);
    int hi = min(p.x + r + 1, int(low_size.x));
    float4 s = 0.0;
    [loop] for (int x = lo; x < hi; x++) {
        s += src.Load(int3(x, p.y, 0));
    }
    return s / float(hi - lo);
}

float4 box_v(int2 p) {
    int r = int(g_radius);
    int lo = max(p.y - r, 0);
    int hi = min(p.y + r + 1, int(low_size.y));
    float4 s = 0.0;
    [loop] for (int y = lo; y < hi; y++) {
        s += src.Load(int3(p.x, y, 0));
    }
    return s / float(hi - lo);
}

// Column means of the row means = the window means of I, p, Ip, II; then
// the guided filter's a and b.
float4 ps_box_v_ab(VsOut i) : SV_Target {
    float4 m = box_v(int2(i.pos.xy));
    float var = max(m.w - m.x * m.x, 0.0);
    float cov = m.z - m.x * m.y;
    float a = cov / (var + g_eps);
    return float4(a, m.y - a * m.x, 0.0, 0.0);
}

// Column means (of the row means of a and b).
float4 ps_box_v(VsOut i) : SV_Target {
    return box_v(int2(i.pos.xy));
}

// q = mean_a * I + mean_b at g_size: mean_a / mean_b (aux) bilinear between
// the block centres, I = the full-size luma (src), rounded to a byte.
float ps_guided_out(VsOut i) : SV_Target {
    int2 p = int2(i.pos.xy);
    float l = round_up(src.Load(int3(p, 0)).r * 255.0);
    float2 c = clamp((float2(p) + 0.5) / 4.0 - 0.5, 0.0, low_size - 1.0);
    int2 i0 = int2(floor(c));
    int2 i1 = min(i0 + 1, int2(low_size) - 1);
    float2 f = c - float2(i0);
    float2 v00 = aux.Load(int3(i0.x, i0.y, 0)).xy;
    float2 v10 = aux.Load(int3(i1.x, i0.y, 0)).xy;
    float2 v01 = aux.Load(int3(i0.x, i1.y, 0)).xy;
    float2 v11 = aux.Load(int3(i1.x, i1.y, 0)).xy;
    float2 r0 = v00 + (v01 - v00) * f.y;
    float2 r1 = v10 + (v11 - v10) * f.y;
    float2 ab = r0 + (r1 - r0) * f.x;
    float q = ab.x * (l * (1.0 / 255.0)) + ab.y;
    return floor(clamp(q * 255.0 + 0.5, 0.0, 255.0)) / 255.0;
}
