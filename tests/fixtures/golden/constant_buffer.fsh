#version 330
layout(std140) uniform Globals { vec4 Color; mat4 View; float Time; };
uniform samplerCube Sky;
out vec4 fragColor;
void main() {
    fragColor = Color + View[0] + texture(Sky, vec3(0.0)) * Time;
}