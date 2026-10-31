#version 330
layout(std140) uniform Globals { vec4 Tint; };
uniform sampler2D Sampler0;
out vec4 fragColor;
void main() {
    fragColor = texture(Sampler0, vec2(0.5)) * Tint;
}
