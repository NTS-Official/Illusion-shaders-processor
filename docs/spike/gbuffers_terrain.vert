#version 450 core

layout(std140, binding = 0) uniform SxDynamicTransforms {
    mat4 ModelViewMat;
    vec4 ColorModulator;
    vec3 ModelOffset;
    mat4 TextureMat;
} iris_transforms;

layout(std140, binding = 2) uniform SxMatrices {
    mat4 iris_ProjMat;
    mat3 iris_NormalMat;
};

layout(std140, binding = 3) uniform SxShadowProjection {
    mat4 shadowProjection;
    mat4 shadowModelView;
};

layout(location = 0) in vec3 iris_Position;
layout(location = 1) in vec4 iris_Color;
layout(location = 2) in vec2 iris_UV0;
layout(location = 3) in ivec2 iris_UV2;
layout(location = 4) in vec3 iris_Normal;

layout(location = 0) out vec2 v_uv;
layout(location = 1) out vec4 v_color;
layout(location = 2) flat out vec3 v_normal;
layout(location = 3) out vec4 v_shadowPos;

// R4 的核心：gl_ModelViewMatrix 的替代（含 ModelOffset 平移）
mat4 sxTranslate(vec3 offset) {
    return mat4(1.0, 0.0, 0.0, 0.0,
                0.0, 1.0, 0.0, 0.0,
                0.0, 0.0, 1.0, 0.0,
                offset.x, offset.y, offset.z, 1.0);
}

void main() {
    vec3 worldPos = iris_Position;
    mat4 modelView = iris_transforms.ModelViewMat * sxTranslate(iris_transforms.ModelOffset);
    vec4 viewPos = modelView * vec4(worldPos, 1.0);

    gl_Position = iris_ProjMat * viewPos;

    v_uv = iris_UV0;
    v_color = iris_Color * iris_transforms.ColorModulator;
    v_normal = normalize(iris_NormalMat * iris_Normal);
    v_shadowPos = shadowProjection * shadowModelView * vec4(worldPos, 1.0);

    // UV2（lightmap）改写后仍要参与运算，验证 ivec2 属性
    float blockLight = float(iris_UV2.x) / 240.0;
    v_color.a *= (0.5 + 0.5 * blockLight);
}
