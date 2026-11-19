// Backdrop passes: the blurred webcam (in-box Blur, Blurred room), the
// cover-scaled box image and the box colour. Mirrors p2c_compositor's
// BlurCache: the webcam is scaled down (cover-cropped for the blurred room)
// with the exact filter, blurred with a separable Gaussian in float (rounded
// to bytes like the CPU), and drawn upscaled with the bilinear sampler.
//
// Compiled after compose.hlsl: uses src (t0), lin (s0), PassConsts (b0:
// rect = destination, src_size), tri() and round_up4().

cbuffer BlurConsts : register(b1) {
    float4 crop;   // ps_cover: source crop left, top, width, height (pixels)
    float4 fill;   // ps_fill: colour (r, g, b, 1)
    uint radius;   // Gaussian radius in taps (kernel = 2 * radius + 1)
    uint3 bpad;
    float4 k[8];   // kernel weights; tap t is k[t / 4][t % 4]
};

float weight(int t) {
    return k[t >> 2][t & 3];
}

// resample_rgba over the crop rectangle of src, as fast_image_resize crops:
// destination centres map into the crop, taps clamp to the whole image.
float4 resample_crop(float2 p, float2 ssize, float2 dsize) {
    float2 scale = crop.zw / dsize;
    float2 fs = max(scale, 1.0);
    float2 c = crop.xy + p * scale;
    int2 lo = max(int2(floor(c - fs)), 0);
    int2 hi = min(int2(ceil(c + fs)), int2(ssize));
    float wy_sum = 0.0;
    for (int y = lo.y; y < hi.y; y++) {
        wy_sum += tri(y, c.y, fs.y);
    }
    float4 acc = 0.0;
    float wx_sum = 0.0;
    [loop] for (int x = lo.x; x < hi.x; x++) {
        float wx = tri(x, c.x, fs.x);
        if (wx > 0.0) {
            float4 col = 0.0;
            [loop] for (int y = lo.y; y < hi.y; y++) {
                col += tri(y, c.y, fs.y) * round_up4(src.Load(int3(x, y, 0)) * 255.0);
            }
            acc += wx * round_up4(col / wy_sum);
            wx_sum += wx;
        }
    }
    return clamp(round_up4(acc / wx_sum), 0.0, 255.0);
}

// src's crop scaled to the destination (exact filter), opaque.
float4 ps_cover(VsOut i) : SV_Target {
    float4 c = resample_crop(i.pos.xy - rect.xy, src_size, rect.zw);
    return float4(c.rgb / 255.0, 1.0);
}

// Horizontal Gaussian of the byte image src into float (0..255 units).
float4 ps_blur_h(VsOut i) : SV_Target {
    int2 p = int2(i.pos.xy - rect.xy);
    int w = int(rect.z);
    int r = int(radius);
    float3 acc = 0.0;
    [loop] for (int t = 0; t <= 2 * r; t++) {
        int sx = clamp(p.x + t - r, 0, w - 1);
        acc += weight(t) * round_up4(src.Load(int3(sx, p.y, 0)) * 255.0).rgb;
    }
    return float4(acc, 1.0);
}

// Vertical Gaussian of the float image src, rounded to bytes, opaque.
float4 ps_blur_v(VsOut i) : SV_Target {
    int2 p = int2(i.pos.xy - rect.xy);
    int h = int(rect.w);
    int r = int(radius);
    float3 acc = 0.0;
    [loop] for (int t = 0; t <= 2 * r; t++) {
        int sy = clamp(p.y + t - r, 0, h - 1);
        acc += weight(t) * src.Load(int3(p.x, sy, 0)).rgb;
    }
    return float4(floor(acc + 0.5) / 255.0, 1.0);
}

// A solid colour (the box colour backdrop).
float4 ps_fill(VsOut i) : SV_Target {
    return fill;
}
