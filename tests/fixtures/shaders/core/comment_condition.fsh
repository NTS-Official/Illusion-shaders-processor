#version 330
#if QUALITY == 0 // Potato graph
float potato;
#else
float fancy;
#endif
#if QUALITY == 0 /* block comment */
float potato_block;
#endif
void main() {}
