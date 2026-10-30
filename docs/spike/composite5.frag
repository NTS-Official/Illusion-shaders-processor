#version 450 core

// ---- 兼容块（具名实例，与 Iris 的 iris_transforms 一一对应）----
layout(std140, binding = 0) uniform SxDynamicTransforms {
    mat4 ModelViewMat;
    vec4 ColorModulator;
    vec3 ModelOffset;
    mat4 TextureMat;
} iris_transforms;

// ---- 散装 uniform（R8：匿名块 + 裸成员名）----
layout(std140, binding = 1) uniform SxLooseUniforms {
    mat4 shadowModelView;
    vec3 cameraPosition;
    float frameTimeCounter;
    vec3 shadowLightPosition;
    float viewWidth;
    float viewHeight;
    vec2 invViewSize;
};

// ---- 分离的矩阵 uniform ----
layout(std140, binding = 2) uniform SxMatrices {
    mat4 iris_ProjMat;
    mat3 iris_NormalMat;
};

// ---- 采样器（M1：保留 combined sampler）----
layout(binding = 100) uniform sampler2D colortex0;
layout(binding = 101) uniform sampler2D colortex1;
layout(binding = 102) uniform sampler2D colortex4;
layout(binding = 103) uniform sampler2D depthtex0;
layout(binding = 104) uniform sampler2D noisetex;
layout(binding = 105) uniform sampler2DShadow shadowtex0;
layout(binding = 106) uniform usampler2D colortex7;

// ---- 阶段 IO（R3：显式 location；R6：MRT 槽位）----
layout(location = 0) in vec2 iris_UV0;
layout(location = 1) in vec4 iris_Color;
layout(location = 2) flat in vec3 iris_Normal;

layout(location = 0) out vec4 iris_FragData0;
layout(location = 1) out vec4 iris_FragData1;

float shadowLookup(vec2 uv, float depth) {
    // sampler2DShadow 的比较采样
    return texture(shadowtex0, vec3(uv, depth));
}

vec3 tonemap(vec3 c) {
    return c / (c + vec3(1.0));
}

void main() {
    vec2 uv = iris_UV0;
    vec4 base = texture(colortex0, uv);
    vec4 extra = textureLod(colortex1, uv, 0.0);
    vec4 aux = texelFetch(colortex4, ivec2(uv * vec2(viewWidth, viewHeight)), 0);
    float depth = texture(depthtex0, uv).r;

    float sh = 0.0;
    if (depth < 1.0) {
        sh = shadowLookup(uv * invViewSize.x * viewWidth, depth);
    }

    vec4 noise = texture(noisetex, uv * frameTimeCounter);
    uvec4 ids = texelFetch(colortex7, ivec2(0), 0);

    vec3 lin = base.rgb * iris_transforms.ColorModulator.rgb * iris_Color.rgb;
    lin += extra.rgb * 0.25;
    lin *= (0.75 + 0.25 * sh);
    lin += aux.rgb * 0.1;
    lin += noise.rgb * 0.001;
    lin += float(ids.x) * 1e-6;

    if (lin.r > 8.0) discard;

    iris_FragData0 = vec4(tonemap(lin), base.a);
    iris_FragData1 = vec4(shadowLightPosition * 0.01, 1.0);
}
