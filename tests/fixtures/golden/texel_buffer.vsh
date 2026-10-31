#version 330
uniform isamplerBuffer Faces;
void main() {
    int face = texelFetch(Faces, gl_VertexIndex).r;
    gl_Position = vec4(float(face));
    gl_PointSize = 2.0;
}