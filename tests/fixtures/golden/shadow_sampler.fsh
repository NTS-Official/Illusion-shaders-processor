#version 330
uniform sampler2DShadow ShadowMap;
out vec4 fragColor;
void main() {
    fragColor = vec4(texture(ShadowMap, vec3(vec2(0.5), 0.25)));
}
