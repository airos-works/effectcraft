/*
 * EffectCraft's Media Kit footage: movies and sounds its own decoders (the
 * FilmCraft codecs) don't read (MPEG-1/2, MPEG-4 Part 2, Windows Media,
 * Theora, VP8, FLAC, Vorbis...), decoded by the system's codecs (the ffmpeg
 * plugin, and hardware decoder plugins where the machine has them) through
 * BMediaFile and BMediaTrack, from the file on disk.
 *
 * src/media_kit.rs offers them to the app (the importer and the footage
 * source). Every call on a handle is made by one Rust thread at a time.
 * After DeckCraft's native/media.cpp, which reads from memory.
 */

#include <Entry.h>
#include <MediaDefs.h>
#include <MediaFile.h>
#include <MediaFormats.h>
#include <MediaTrack.h>

#include <new>
#include <stdlib.h>
#include <string.h>


extern "C" {

// What ec_media_probe found; the strings are NUL-terminated.
struct EcMediaInfo {
	char container[32];
	int64_t duration_us;
	int32_t has_video;
	int32_t video_decodable;
	int32_t width;
	int32_t height;
	float fps;
	int64_t frames;
	char video_codec[96];
	int32_t has_audio;
	int32_t audio_decodable;
	int32_t channels;
	float rate;
	char audio_codec[96];
};

}


namespace {


BMediaFile*
OpenFile(const char* path)
{
	if (path == NULL)
		return NULL;
	entry_ref ref;
	if (get_ref_for_path(path, &ref) != B_OK)
		return NULL;
	BMediaFile* file = new(std::nothrow) BMediaFile(&ref);
	if (file != NULL && file->InitCheck() != B_OK) {
		delete file;
		return NULL;
	}
	return file;
}


// The first track of the type (B_MEDIA_ENCODED_VIDEO or _AUDIO), or NULL;
// release it with file->ReleaseTrack().
BMediaTrack*
FirstTrack(BMediaFile* file, media_type type, media_format* encoded)
{
	int32 count = file->CountTracks();
	for (int32 i = 0; i < count; i++) {
		BMediaTrack* track = file->TrackAt(i);
		if (track == NULL)
			continue;
		if (track->EncodedFormat(encoded) == B_OK && encoded->type == type)
			return track;
		file->ReleaseTrack(track);
	}
	return NULL;
}


// Asks the track's decoder for B_RGB32 frames of the track's size.
status_t
NegotiateVideo(BMediaTrack* track, const media_format& encoded,
	media_format* decoded)
{
	uint32 width = encoded.u.encoded_video.output.display.line_width;
	uint32 height = encoded.u.encoded_video.output.display.line_count;
	decoded->Clear();
	decoded->type = B_MEDIA_RAW_VIDEO;
	decoded->u.raw_video = media_raw_video_format::wildcard;
	decoded->u.raw_video.display.format = B_RGB32;
	decoded->u.raw_video.display.line_width = width;
	decoded->u.raw_video.display.line_count = height;
	decoded->u.raw_video.display.bytes_per_row = width * 4;
	status_t status = track->DecodedFormat(decoded);
	if (status != B_OK)
		return status;
	const media_video_display_info& display = decoded->u.raw_video.display;
	if (display.format != B_RGB32 || display.line_width == 0
		|| display.line_count == 0 || display.line_width > 16384
		|| display.line_count > 16384
		|| display.bytes_per_row < display.line_width * 4)
		return B_MEDIA_BAD_FORMAT;
	return B_OK;
}


// Asks the track's decoder for interleaved float samples.
status_t
NegotiateAudio(BMediaTrack* track, media_format* decoded)
{
	decoded->Clear();
	decoded->type = B_MEDIA_RAW_AUDIO;
	decoded->u.raw_audio = media_raw_audio_format::wildcard;
	decoded->u.raw_audio.format = media_raw_audio_format::B_AUDIO_FLOAT;
	decoded->u.raw_audio.byte_order = B_MEDIA_HOST_ENDIAN;
	status_t status = track->DecodedFormat(decoded);
	if (status != B_OK)
		return status;
	const media_raw_audio_format& raw = decoded->u.raw_audio;
	if (raw.channel_count < 1 || raw.channel_count > 32
		|| !(raw.frame_rate >= 1000 && raw.frame_rate <= 768000)
		|| raw.byte_order != B_MEDIA_HOST_ENDIAN)
		return B_MEDIA_BAD_FORMAT;
	switch (raw.format) {
		case media_raw_audio_format::B_AUDIO_FLOAT:
		case media_raw_audio_format::B_AUDIO_INT:
		case media_raw_audio_format::B_AUDIO_SHORT:
		case media_raw_audio_format::B_AUDIO_UCHAR:
		case media_raw_audio_format::B_AUDIO_CHAR:
			return B_OK;
		default:
			return B_MEDIA_BAD_FORMAT;
	}
}


void
CodecName(BMediaTrack* track, char* name, size_t size)
{
	media_codec_info info;
	if (track->GetCodecInfo(&info) == B_OK)
		strlcpy(name, info.pretty_name[0] != '\0' ? info.pretty_name
			: info.short_name, size);
}


// One sample of a raw format as a float.
float
Sample(const uint8* data, uint32 format, size_t index)
{
	switch (format) {
		case media_raw_audio_format::B_AUDIO_FLOAT:
			return ((const float*)data)[index];
		case media_raw_audio_format::B_AUDIO_INT:
			return ((const int32*)data)[index] / 2147483648.0f;
		case media_raw_audio_format::B_AUDIO_SHORT:
			return ((const int16*)data)[index] / 32768.0f;
		case media_raw_audio_format::B_AUDIO_UCHAR:
			return (((const uint8*)data)[index] - 128) / 128.0f;
		case media_raw_audio_format::B_AUDIO_CHAR:
			return ((const int8*)data)[index] / 128.0f;
	}
	return 0;
}


size_t
SampleSize(uint32 format)
{
	return format & media_raw_audio_format::B_AUDIO_SIZE_MASK;
}


// A video track being read: the decoder, and a buffer for its frames.
struct Video {
	BMediaFile* file;
	BMediaTrack* track;
	uint8* buffer;
	size_t bufferSize;
	uint32 width;
	uint32 height;
	uint32 bytesPerRow;
};


}	// namespace


extern "C" {


// Describes the media at path. Returns 0, or -1 when the Media Kit can't
// read it.
int32_t
ec_media_probe(const char* path, EcMediaInfo* info)
{
	memset(info, 0, sizeof(*info));
	BMediaFile* file = OpenFile(path);
	if (file == NULL)
		return -1;
	media_file_format fileFormat;
	if (file->GetFileFormatInfo(&fileFormat) == B_OK)
		strlcpy(info->container, fileFormat.short_name, sizeof(info->container));

	media_format encoded;
	BMediaTrack* track = FirstTrack(file, B_MEDIA_ENCODED_VIDEO, &encoded);
	if (track != NULL) {
		info->has_video = 1;
		info->width = encoded.u.encoded_video.output.display.line_width;
		info->height = encoded.u.encoded_video.output.display.line_count;
		info->fps = encoded.u.encoded_video.output.field_rate;
		info->frames = track->CountFrames();
		info->duration_us = track->Duration();
		CodecName(track, info->video_codec, sizeof(info->video_codec));
		media_format decoded;
		if (NegotiateVideo(track, encoded, &decoded) == B_OK) {
			info->video_decodable = 1;
			info->width = decoded.u.raw_video.display.line_width;
			info->height = decoded.u.raw_video.display.line_count;
		}
		file->ReleaseTrack(track);
	}
	track = FirstTrack(file, B_MEDIA_ENCODED_AUDIO, &encoded);
	if (track != NULL) {
		info->has_audio = 1;
		info->channels = encoded.u.encoded_audio.output.channel_count;
		info->rate = encoded.u.encoded_audio.output.frame_rate;
		if (track->Duration() > info->duration_us)
			info->duration_us = track->Duration();
		CodecName(track, info->audio_codec, sizeof(info->audio_codec));
		media_format decoded;
		if (NegotiateAudio(track, &decoded) == B_OK) {
			info->audio_decodable = 1;
			info->channels = decoded.u.raw_audio.channel_count;
			info->rate = decoded.u.raw_audio.frame_rate;
		}
		file->ReleaseTrack(track);
	}
	delete file;
	return 0;
}


// Opens the video track at path for decoding to B_RGB32 frames of
// *width × *height. NULL when there's none the Media Kit decodes.
void*
ec_video_open(const char* path, int32_t* width, int32_t* height)
{
	*width = 0;
	*height = 0;
	BMediaFile* file = OpenFile(path);
	if (file == NULL)
		return NULL;
	media_format encoded;
	media_format decoded;
	BMediaTrack* track = FirstTrack(file, B_MEDIA_ENCODED_VIDEO, &encoded);
	if (track == NULL || NegotiateVideo(track, encoded, &decoded) != B_OK) {
		if (track != NULL)
			file->ReleaseTrack(track);
		delete file;
		return NULL;
	}
	const media_video_display_info& display = decoded.u.raw_video.display;
	Video* video = new(std::nothrow) Video;
	size_t bufferSize = (size_t)display.bytes_per_row * display.line_count;
	uint8* buffer = (uint8*)malloc(bufferSize);
	if (video == NULL || buffer == NULL) {
		delete video;
		free(buffer);
		file->ReleaseTrack(track);
		delete file;
		return NULL;
	}
	video->file = file;
	video->track = track;
	video->buffer = buffer;
	video->bufferSize = bufferSize;
	video->width = display.line_width;
	video->height = display.line_count;
	video->bytesPerRow = display.bytes_per_row;
	*width = video->width;
	*height = video->height;
	return video;
}


// Decodes the next frame into rgba (width × height × 4 bytes, opaque RGBA)
// and its presentation time (microseconds). Returns 1, 0 at the end of the
// track, or -1 on an error.
int32_t
ec_video_next(void* handle, uint8_t* rgba, size_t length, int64_t* time)
{
	Video* video = (Video*)handle;
	if (video == NULL || length < (size_t)video->width * video->height * 4)
		return -1;
	int64 count = 1;
	media_header header;
	memset(&header, 0, sizeof(header));
	status_t status = video->track->ReadFrames(video->buffer, &count, &header);
	if (status == B_LAST_BUFFER_ERROR || (status == B_OK && count == 0))
		return 0;
	if (status != B_OK)
		return -1;
	*time = header.start_time;
	for (uint32 y = 0; y < video->height; y++) {
		const uint8* in = video->buffer + (size_t)y * video->bytesPerRow;
		uint8* out = rgba + (size_t)y * video->width * 4;
		for (uint32 x = 0; x < video->width; x++) {
			// B_RGB32 is B, G, R, unused in memory.
			out[0] = in[2];
			out[1] = in[1];
			out[2] = in[0];
			out[3] = 255;
			in += 4;
			out += 4;
		}
	}
	return 1;
}


// Moves to the key frame at or before time (microseconds); returns the time
// decoding resumes at.
int64_t
ec_video_seek(void* handle, int64_t time)
{
	Video* video = (Video*)handle;
	if (video == NULL)
		return 0;
	bigtime_t at = time;
	if (video->track->SeekToTime(&at, B_MEDIA_SEEK_CLOSEST_BACKWARD) != B_OK) {
		at = 0;
		video->track->SeekToTime(&at, B_MEDIA_SEEK_CLOSEST_BACKWARD);
	}
	return at;
}


void
ec_video_close(void* handle)
{
	Video* video = (Video*)handle;
	if (video == NULL)
		return;
	video->file->ReleaseTrack(video->track);
	delete video->file;
	free(video->buffer);
	delete video;
}


// Decodes the whole audio track at path into interleaved float samples
// (*channels per frame at *rate Hz), at most maxFrames frames. *samples is
// malloc'd (free with free()); its length in frames is the return value, or
// -1 when there's no audio the Media Kit decodes.
int64_t
ec_audio_decode(const char* path, int64_t maxFrames, float** samples,
	int32_t* channels, float* rate)
{
	*samples = NULL;
	*channels = 0;
	*rate = 0;
	BMediaFile* file = OpenFile(path);
	if (file == NULL)
		return -1;
	media_format encoded;
	media_format decoded;
	BMediaTrack* track = FirstTrack(file, B_MEDIA_ENCODED_AUDIO, &encoded);
	if (track == NULL || NegotiateAudio(track, &decoded) != B_OK) {
		if (track != NULL)
			file->ReleaseTrack(track);
		delete file;
		return -1;
	}
	const media_raw_audio_format& raw = decoded.u.raw_audio;
	size_t sampleSize = SampleSize(raw.format);
	uint32 ch = raw.channel_count;
	// ReadFrames fills up to buffer_size bytes; leave room for decoders that
	// return more than they said.
	size_t bufferSize = raw.buffer_size > 0 ? raw.buffer_size * 4 : 1 << 20;
	if (bufferSize < 1 << 16)
		bufferSize = 1 << 16;
	uint8* buffer = (uint8*)malloc(bufferSize);
	size_t capacity = 0;
	int64 frames = 0;
	float* out = NULL;
	bool failed = buffer == NULL || sampleSize == 0;
	while (!failed && frames < maxFrames) {
		int64 count = 0;
		media_header header;
		status_t status = track->ReadFrames(buffer, &count, &header);
		if (status != B_OK || count <= 0)
			break;
		if ((size_t)count * ch * sampleSize > bufferSize)
			break;
		if (count > maxFrames - frames)
			count = maxFrames - frames;
		if ((size_t)(frames + count) * ch > capacity) {
			size_t want = capacity == 0 ? (size_t)count * ch * 8 : capacity * 2;
			while (want < (size_t)(frames + count) * ch)
				want *= 2;
			float* grown = (float*)realloc(out, want * sizeof(float));
			if (grown == NULL) {
				failed = true;
				break;
			}
			out = grown;
			capacity = want;
		}
		for (size_t i = 0; i < (size_t)count * ch; i++)
			out[(size_t)frames * ch + i] = Sample(buffer, raw.format, i);
		frames += count;
	}
	free(buffer);
	file->ReleaseTrack(track);
	delete file;
	if (failed || frames == 0) {
		free(out);
		return -1;
	}
	*samples = out;
	*channels = ch;
	*rate = raw.frame_rate;
	return frames;
}

}
