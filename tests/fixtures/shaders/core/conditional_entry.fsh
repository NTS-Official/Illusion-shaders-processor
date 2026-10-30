#version 330

void main() {
#if defined(A) && A == 1
    float branch_a;
#elif defined(B)
    float branch_b;
#else
    float branch_c;
#endif
}
