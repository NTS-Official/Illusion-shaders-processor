#version 330
#extension GL_ARB_separate_shader_objects : require

#include <minecraft:helpers.glsl>

void main() {
#ifdef ENABLED
    float enabled;
#else
    float disabled;
#endif
}
