#version 330
layout(location = 0) in vec3 Position;
layout(location = 1) in vec4 Color;
layout(location = 2) in vec2 UV0;
out vec4 vertexColor;
void main() {
    vertexColor = Color;
    gl_Position = vec4(Position + vec3(UV0, 0.0), 1.0);
}
