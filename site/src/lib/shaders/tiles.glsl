uniform sampler2D video;
uniform ivec2 grid;
uniform int tile;

vec3 pick(int index) {
	vec2 cell = vec2(float(index % grid.x), float(index / grid.x));
	vec2 size = vec2(textureSize(video, 0)) / vec2(grid);
	return texture(video, (cell + inside(size)) / vec2(grid)).rgb;
}

void main() {
	color = vec4(look(pick(tile), pick(0)), 1.0);
}
