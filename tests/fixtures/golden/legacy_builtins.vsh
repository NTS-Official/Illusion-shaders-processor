#version 330

out vec4 vertexColor;
out vec2 texCoord;

void main() {
    gl_Position = ftransform();
    texCoord = (gl_TextureMatrix[0] * gl_MultiTexCoord0).xy;
    vertexColor = gl_Color * vec4(gl_Normal, 1.0) + gl_Vertex;
}