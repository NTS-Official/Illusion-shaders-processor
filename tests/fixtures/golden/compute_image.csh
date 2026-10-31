#version 450 core
layout(local_size_x = 8, local_size_y = 8, local_size_z = 8) in;

writeonly uniform image3D floodfill;

void main() {
    imageStore(floodfill, ivec3(gl_GlobalInvocationID), vec4(1.0));
}
