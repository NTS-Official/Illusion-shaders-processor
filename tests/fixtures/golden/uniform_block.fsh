#version 330

uniform float far;
uniform mat4 gbufferProjection;
uniform vec4 fogColor = vec4(1.0);

out vec4 fragColor;

void main() {
    fragColor = fogColor * (far + gbufferProjection[0][0]);
}