#ifndef WASAPI_EXCLUSIVE_V3_H
#define WASAPI_EXCLUSIVE_V3_H

#ifdef _MSC_VER
#define WASAPI_CALL __cdecl
#else
#define WASAPI_CALL
#endif

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Opaque session handle. Keep it and pass it to each subsequent call.
 * Use sequentially from the same LabVIEW execution thread; do not close or
 * use a session concurrently.
 */
int WASAPI_CALL wasapi_open(
    void **outSession,
    int sampleRate,
    int channels,
    int bitsPerSample,
    int bufferMs);

/* dataLenFrames is a count of interleaved frames, not individual samples. */
int WASAPI_CALL wasapi_write(
    void *session,
    const void *data,
    int dataLenFrames);

int WASAPI_CALL wasapi_get_free_space(
    void *session,
    int *pFramesFree);

/* The handle is invalid after this call. Do not use it concurrently. */
int WASAPI_CALL wasapi_close(void *session);

/* Initializes a separate beep session; pass its handle to all other beep calls. */
int WASAPI_CALL wasapi_beep_init(void **outBeepSession);
/* wasapi_beep_close invalidates beepSession. */
int WASAPI_CALL wasapi_beep(
    void *beepSession,
    int durationMs,
    int frequencyHz,
    double amplitude);
int WASAPI_CALL wasapi_beep_on(
    void *beepSession,
    int frequencyHz,
    double amplitude);
int WASAPI_CALL wasapi_beep_off(void *beepSession);
int WASAPI_CALL wasapi_beep_close(void *beepSession);

#ifdef __cplusplus
}
#endif

#undef WASAPI_CALL

#endif
