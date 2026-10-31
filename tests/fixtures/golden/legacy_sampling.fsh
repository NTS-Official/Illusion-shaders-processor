#version 330

uniform sampler2D colortex;
uniform sampler2DShadow shadowtex0;

out vec4 fragColor;

void main() {
    vec4 lod = texture2DLod(colortex, vec2(0.5), 0.0);
    float shadow = shadow2D(shadowtex0, vec3(0.5)).x;
    fragColor = lod * shadow;
}