/* The reference for scw-capture's hand-written PipeWire parameters (spa.rs):
 * the same pods built with SPA's own builder macros, printed as hex. CI
 * compiles it against the system's SPA headers and tests/spa_oracle.rs
 * compares the bytes.
 *
 *   cc -o spa_oracle tests/spa_oracle.c $(pkg-config --cflags libspa-0.2)
 */
#include <stdio.h>

#include <spa/param/buffers.h>
#include <spa/param/video/format-utils.h>
#include <spa/pod/builder.h>

static void dump(const char *name, const struct spa_pod *p)
{
	const unsigned char *b = (const unsigned char *)p;
	unsigned int n = SPA_POD_SIZE(p);
	printf("%s ", name);
	for (unsigned int i = 0; i < n; i++)
		printf("%02x", b[i]);
	printf("\n");
}

int main(void)
{
	unsigned char buffer[4096];
	struct spa_pod_builder b = SPA_POD_BUILDER_INIT(buffer, sizeof(buffer));

	const struct spa_pod *offer = spa_pod_builder_add_object(&b,
		SPA_TYPE_OBJECT_Format, SPA_PARAM_EnumFormat,
		SPA_FORMAT_mediaType, SPA_POD_Id(SPA_MEDIA_TYPE_video),
		SPA_FORMAT_mediaSubtype, SPA_POD_Id(SPA_MEDIA_SUBTYPE_raw),
		SPA_FORMAT_VIDEO_format, SPA_POD_CHOICE_ENUM_Id(9,
			SPA_VIDEO_FORMAT_BGRx, SPA_VIDEO_FORMAT_BGRx,
			SPA_VIDEO_FORMAT_BGRA, SPA_VIDEO_FORMAT_RGBx,
			SPA_VIDEO_FORMAT_RGBA, SPA_VIDEO_FORMAT_xRGB,
			SPA_VIDEO_FORMAT_xBGR, SPA_VIDEO_FORMAT_ARGB,
			SPA_VIDEO_FORMAT_ABGR),
		SPA_FORMAT_VIDEO_size, SPA_POD_CHOICE_RANGE_Rectangle(
			&SPA_RECTANGLE(1920, 1080),
			&SPA_RECTANGLE(1, 1),
			&SPA_RECTANGLE(16384, 16384)),
		SPA_FORMAT_VIDEO_framerate, SPA_POD_CHOICE_RANGE_Fraction(
			&SPA_FRACTION(30, 1),
			&SPA_FRACTION(0, 1),
			&SPA_FRACTION(1000, 1)));
	dump("enum_format", offer);

	const struct spa_pod *buffers = spa_pod_builder_add_object(&b,
		SPA_TYPE_OBJECT_ParamBuffers, SPA_PARAM_Buffers,
		SPA_PARAM_BUFFERS_dataType, SPA_POD_CHOICE_FLAGS_Int(
			(1 << SPA_DATA_MemPtr) | (1 << SPA_DATA_MemFd)));
	dump("buffers", buffers);

	/* A settled format as a producer sends one, for the parser. */
	struct spa_video_info_raw info = {
		.format = SPA_VIDEO_FORMAT_BGRA,
		.size = SPA_RECTANGLE(5120, 1440),
		.framerate = SPA_FRACTION(60, 1),
	};
	dump("format", spa_format_video_raw_build(&b, SPA_PARAM_Format, &info));
	return 0;
}
