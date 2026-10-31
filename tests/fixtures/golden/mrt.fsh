#version 330

/* DRAWBUFFERS:0364 */

layout(std140) uniform Globals {
    vec4 Tint;
};

void main() {
    gl_FragData[0] = Tint;
    gl_FragData[3] = vec4(1.0);
}
