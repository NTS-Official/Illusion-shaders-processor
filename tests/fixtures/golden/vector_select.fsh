#version 330
layout(location = 0) out vec4 fragColor;

void main() {
    bvec3 mask = bvec3(true, false, true);
    vec3 a = vec3(1.0, 2.0, 3.0);
    vec3 b = vec3(4.0, 5.0, 6.0);
    fragColor = vec4(mix(a, b, mask), 1.0);
}
