#version 330
#include "/lib/typed.glsl"

out vec4 fragColor;

void main() {
    fragColor = vec4(typed(vec3(1.0)));
}
