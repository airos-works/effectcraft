/*
 * EffectCraft's audio preview output on the Media Kit: a BSoundPlayer node
 * playing float stereo into the system mixer (the desktop app uses cpal).
 * The Media Kit owns the callback thread; ec_audio_stop() joins it
 * (Stop(true)) before the Rust side frees the cookie.
 */

#include <MediaDefs.h>
#include <SoundPlayer.h>

#include <new>
#include <string.h>


struct EcAudio {
	BSoundPlayer* player;
	void* cookie;
	void (*fill)(void*, float*, size_t, size_t, uint32_t);
};


static void
FillAudio(void* cookie, void* buffer, size_t bytes,
	const media_raw_audio_format& format)
{
	memset(buffer, 0, bytes);
	EcAudio* audio = static_cast<EcAudio*>(cookie);
	if (format.format == media_raw_audio_format::B_AUDIO_FLOAT
		&& format.channel_count > 0) {
		audio->fill(audio->cookie, static_cast<float*>(buffer),
			bytes / sizeof(float), format.channel_count,
			static_cast<uint32_t>(format.frame_rate));
	}
}


extern "C" {

// Opens a player asking for `rate` Hz float stereo; NULL without a usable
// Media Kit (no media_server, no output). Not started yet.
void*
ec_audio_open(uint32_t rate, const char* name, void* cookie,
	void (*fill)(void*, float*, size_t, size_t, uint32_t))
{
	EcAudio* audio = new(std::nothrow) EcAudio;
	if (audio == NULL)
		return NULL;
	audio->cookie = cookie;
	audio->fill = fill;
	media_raw_audio_format format = media_raw_audio_format::wildcard;
	format.format = media_raw_audio_format::B_AUDIO_FLOAT;
	format.channel_count = 2;
	format.frame_rate = rate > 0 ? rate : 48000;
	format.byte_order = B_MEDIA_HOST_ENDIAN;
	audio->player = new(std::nothrow) BSoundPlayer(&format, name, FillAudio,
		NULL, audio);
	if (audio->player == NULL || audio->player->InitCheck() != B_OK) {
		delete audio->player;
		delete audio;
		return NULL;
	}
	return audio;
}


// The rate the player got (the mixer converts to the device's).
uint32_t
ec_audio_rate(void* handle)
{
	EcAudio* audio = static_cast<EcAudio*>(handle);
	if (audio == NULL)
		return 0;
	media_raw_audio_format format = audio->player->Format();
	return static_cast<uint32_t>(format.frame_rate);
}


int32_t
ec_audio_start(void* handle)
{
	EcAudio* audio = static_cast<EcAudio*>(handle);
	if (audio == NULL || audio->player->Start() != B_OK)
		return -1;
	audio->player->SetHasData(true);
	return 0;
}


// The node's latency (microseconds): what was handed over until it is heard.
int64_t
ec_audio_latency(void* handle)
{
	EcAudio* audio = static_cast<EcAudio*>(handle);
	if (audio == NULL)
		return 0;
	return audio->player->Latency();
}


void
ec_audio_close(void* handle)
{
	EcAudio* audio = static_cast<EcAudio*>(handle);
	if (audio == NULL)
		return;
	audio->player->Stop(true, true);
	delete audio->player;
	delete audio;
}

}
