#version 330

uniform sampler2D Sampler0;

layout(std140) uniform Globals {
    vec4 Color;
};

out vec4 fragColor;

void main() {
    ivec2 size = textureSize(Sampler0, 0);
    fragColor = vec4(Color.rgb * (float(size.x) + float(size.y)), 1.0);
}