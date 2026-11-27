#ifndef WASAPI_EXCLUSIVE_V4_H
#define WASAPI_EXCLUSIVE_V4_H

#ifdef _MSC_VER
#define WASAPI_CALL __cdecl
#else
#define WASAPI_CALL
#endif

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Initializes one exclusive beep session on the default multimedia render endpoint.
 * The session uses event-driven WASAPI buffering and remains exclusive until
 * wasapi_beep_close. Exclusivity applies only to this endpoint.
 * Use sequentially from the same LabVIEW execution thread.
 */
int WASAPI_CALL wasapi_beep_init(void **outBeepSession);
/* Blocks until the requested tone has finished playing. */
int WASAPI_CALL wasapi_beep(
    void *beepSession,
    int freqHz,
    double niv,
    int dureeMs);
int WASAPI_CALL wasapi_beep_on(
    void *beepSession,
    int freqHz,
    double niv);
/* Blocks until the 5 ms fade-out has played; the session remains open. */
int WASAPI_CALL wasapi_beep_off(void *beepSession);
/* Closes the session; call separately after wasapi_beep_off when finished. */
int WASAPI_CALL wasapi_beep_close(void *beepSession);

#ifdef __cplusplus
}
#endif

#undef WASAPI_CALL

#endif
