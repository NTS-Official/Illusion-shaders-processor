#version 330

layout(std140) uniform Globals {
    vec4 Tint;
};

layout(location = 0) in vec2 texCoord;
layout(location = 0) out vec4 fragColor;

void main() {
    float level = texCoord.y * 16.0;
    fragColor = Tint * level;
}
