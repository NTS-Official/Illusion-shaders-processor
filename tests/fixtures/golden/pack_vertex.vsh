#version 120
attribute vec3 Position;
uniform mat4 projectionMatrix;
uniform mat4 modelViewMatrix;
void main() {
    vec3 world = Position + vaPosition;
    vec4 clip = projectionMatrix * modelViewMatrix * vec4(world, 1.0);
    if (gl_VertexID == 0) clip.y += 0.001;
    gl_Position = clip;
}
