#version 300 es
precision highp float;
precision highp int;

in vec2 uv;
out vec4 color;
uniform int view;

float luma(vec3 rgb) {
	return dot(rgb, vec3(0.2126, 0.7152, 0.0722));
}

vec2 inside(vec2 size) {
	return clamp(uv, 0.5 / size, 1.0 - 0.5 / size);
}

vec3 look(vec3 picture, vec3 reference) {
	if (view == 1) return min(abs(picture - reference) * 3.0, 1.0);
	return picture;
}
