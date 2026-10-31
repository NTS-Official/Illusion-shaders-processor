#version 330
in vec3 Position;
in vec4 Color;
in vec2 UV0;
uniform sampler2D Sampler;
out vec4 vertexColor;
void main() {
    vertexColor = Color;
    gl_Position = vec4(Position, 1.0);
}