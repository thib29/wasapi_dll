#ifndef WASAPI_EXCLUSIVE_H
#define WASAPI_EXCLUSIVE_H

#ifdef __cplusplus
extern "C" {
#endif

int __cdecl wasapi_beep(int frequencyHz, int durationMs, double amplitude);
/* Amplitude is between 0.0 and 1.0; call init once and close when finished. */
int __cdecl wasapi_beep_init(void);
int __cdecl wasapi_beep_on(int frequencyHz, double amplitude);
int __cdecl wasapi_beep_off(void);
int __cdecl wasapi_beep_close(void);

#ifdef __cplusplus
}
#endif

#endif
