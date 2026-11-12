const int most = 129;
const float one = 32768.0;

uniform sampler2D atlas;
uniform sampler2D weights;
uniform ivec2 grid;
uniform int frames;
uniform int count;
uniform int center;
uniform int stride;
uniform float gamma;

vec3 encoded(int index) {
	int clamped = clamp(index, 0, frames - 1);
	vec2 cell = vec2(float(clamped % grid.x), float(clamped / grid.x));
	vec2 size = vec2(textureSize(atlas, 0)) / vec2(grid);
	vec3 rgb = texture(atlas, (cell + inside(size)) / vec2(grid)).rgb;
	float y = luma(rgb);
	vec3 yuv = vec3(16.0 + 219.0 * y, 128.0 + 224.0 * (rgb.b - y) / 1.8556, 128.0 + 224.0 * (rgb.r - y) / 1.5748);
	return floor(yuv + 0.5);
}

vec3 decoded(vec3 yuv) {
	float y = (yuv.x - 16.0) / 219.0;
	vec2 c = (yuv.yz - 128.0) / 224.0;
	return clamp(vec3(y + 1.5748 * c.y, y - 0.187324 * c.x - 0.468124 * c.y, y + 1.8556 * c.x), 0.0, 1.0);
}

void main() {
	int first = center - center % stride - (count - 1) / 2 * stride;
	vec3 sums = vec3(float(64 + count / 2));
	float brightness = 0.0;
	for (int at = 0; at < most; at++) {
		if (at >= count) break;
		vec3 tap = encoded(first + at * stride);
		float weight = texelFetch(weights, ivec2(at, 0), 0).r;
		sums += floor(tap * weight / 256.0);
		brightness += weight / one * pow(tap.x / 255.0, gamma);
	}
	vec3 blended = floor(sums / 128.0);
	if (gamma != 1.0) blended.x = floor(pow(brightness, 1.0 / gamma) * 255.0 + 0.5);
	color = vec4(look(decoded(blended), decoded(encoded(center))), 1.0);
}
