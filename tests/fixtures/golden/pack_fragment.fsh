#version 120
#define square(x) ((x) * (x))
uniform sampler2D gcolor;
uniform bool fogEnabled;
/* DRAWBUFFERS:01 */
in vec2 texcoord;
void main() {
    vec4 albedo = texture2D(gcolor, texcoord);
    float fog = (gl_Fog.start * square(texcoord.x)) / max(gl_Fog.scale, 0.001);
    gl_FragData[0] = albedo * (fogEnabled ? 1.0 : 0.5);
    gl_FragData[1] = vec4(fog, gl_Fog.color.rgb * fog);
}
