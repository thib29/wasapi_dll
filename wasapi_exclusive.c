// ============================================================================
// wasapi_exclusive.dll - Access WASAPI mode EXCLUSIF appelable depuis LabVIEW
//
// API simple (types plats) -> palette LabVIEW "Connectivite > Appel de fonction
// de bibliotheque" (Call Library Function Node) :
//
//   wasapi_open(handle*, sampleRate, channels, bitsPerSample, bufferMs)
//   wasapi_write(handle, int16* data, sizeInSamples)   // ecriture bloquante
//   wasapi_get_free_space(handle, &pFramesFree)
//   wasapi_close(handle)
//   wasapi_beep_init()
//   wasapi_beep(frequencyHz, durationMs, amplitude)
//   wasapi_beep_on(frequencyHz, amplitude) / wasapi_beep_off()
//   wasapi_beep_close()
//
// Codes d'erreur : 0 = OK, sinon valeur negatif ou HRESULT adverse.
//
// Compilation Visual Studio (x86 ou x64 selon votre LabVIEW) :
//   cl /LD /O2 wasapi_exclusive.c /Fe:wasapi_exclusive.dll \
//        ole32.lib user32.lib avrt.lib
//
// Compilation MinGW-w64 :
//   gcc -shared -O2 -o wasapi_exclusive.dll wasapi_exclusive.c \
//        -lole32 -luser32
//
// IMPORTANT : l'architecture de la DLL DOIT correspondre au bitness de
// LabVIEW (LabVIEW 32 bits -> DLL 32 bits ; LabVIEW 64 bits -> DLL 64 bits).
// ============================================================================

#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <mmdeviceapi.h>
#include <audioclient.h>
#include <audiosessiontypes.h>
#include <math.h>
#include <stdint.h>
#include <string.h>

static const IID IID_IAudioRenderClientLocal =
    { 0xF294ACFC, 0x3146, 0x4483,
      { 0xA7, 0xBF, 0xAD, 0xDC, 0xA7, 0xC2, 0x60, 0xE2 } };

static volatile LONG g_beepInitialized = 0;
static volatile LONG g_beepFrequency = 0;
static volatile LONG g_beepAmplitude = 0;
static volatile LONG g_beepMode = 0;
static volatile LONG g_beepRemainingFrames = 0;
static volatile LONG g_beepStop = 0;
static volatile LONG g_beepResult = 0;
static HANDLE g_beepThread = NULL;
static HANDLE g_beepReadyEvent = NULL;
static HANDLE g_beepDoneEvent = NULL;

#define ERR_OK              0
#define ERR_BAD_HANDLE    (-1)
#define ERR_BAD_ARGS      (-2)
#define ERR_COINIT        (-3)
#define ERR_NO_DEVICE     (-4)
#define ERR_ACTIVATE      (-5)
#define ERR_INIT          (-6)
#define ERR_FORMAT        (-7)
#define ERR_GETSERVICE    (-8)
#define ERR_STARTRUN      (-9)
#define ERR_WRITE        (-10)

static const CLSID CLSID_MMDeviceEnumerator_IID =
    { 0xBCDE0395, 0xE52F, 0x467C,
      { 0x8E, 0x3D, 0xC4, 0x57, 0x92, 0x91, 0x69, 0x2E } }; // CLSID_MMDeviceEnumerator
static const IID IID_IMMDeviceEnumerator_IID =
    { 0xA95664D2, 0x9614, 0x4F35,
      { 0xA7, 0x46, 0xDE, 0x8D, 0xB6, 0x36, 0x17, 0xE6 } }; // IID_IMMDeviceEnumerator
static const IID IID_IAudioClient_IID =
    { 0x1CB9AD4C, 0xDBFA, 0x4C32,
      { 0xB1, 0x78, 0xC2, 0xF5, 0x68, 0xA7, 0x03, 0xB2 } }; // IID_IAudioClient
typedef struct {
    IAudioClient* client;
    IAudioRenderClient* render;
    WAVEFORMATEX* fmt;
    UINT32        framesPerBuf;
    UINT32        frameSize;     // bytes par frame
} WasapiSession;

static BOOL com_enter(BOOL* need_uninitialize)
{
    HRESULT hr;

    *need_uninitialize = FALSE;
    hr = CoInitializeEx(NULL, COINIT_MULTITHREADED);
    if (hr == S_OK) {
        *need_uninitialize = TRUE;
        return TRUE;
    }
    if (hr == S_FALSE || hr == RPC_E_CHANGED_MODE)
        return TRUE;
    return FALSE;
}

static void com_leave(BOOL need_uninitialize)
{
    if (need_uninitialize)
        CoUninitialize();
}

// ---------------------------------------------------------------------------
// Ouverture en mode EXCLUSIF sur le peripherique de lecture par defaut.
// bitsPerSample : 16 ou 24 uniquement (PCM entier, requis en exclusif).
// Retourne 0 + handle via *outSession.
// ---------------------------------------------------------------------------
__declspec(dllexport)
int32_t __cdecl wasapi_open(void** outSession,
                            int32_t sampleRate,
                            int32_t channels,
                            int32_t bitsPerSample,
                            int32_t bufferMs)
{
    WasapiSession* s = NULL;
    IMMDeviceEnumerator* enumr = NULL;
    IMMDevice* dev = NULL;
    HRESULT hr;
    REFERENCE_TIME dur;
    int32_t rc = ERR_OK;
    BOOL needComUninitialize = FALSE;
    UINT32 blockAlign;
    UINT32 bufferSize;

    if (!outSession) return ERR_BAD_ARGS;
    *outSession = NULL;
    if (sampleRate <= 0 || channels <= 0 ||
        (bitsPerSample != 16 && bitsPerSample != 24) || bufferMs <= 0)
        return ERR_BAD_ARGS;

    if (!com_enter(&needComUninitialize)) return ERR_COINIT;

    s = (WasapiSession*)HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, sizeof(WasapiSession));
    if (!s) {
        com_leave(needComUninitialize);
        return ERR_NO_DEVICE;
    }

    blockAlign = (UINT32)channels * ((UINT32)bitsPerSample / 8);
    if (blockAlign == 0 || blockAlign > 0xFFFF ||
        (UINT64)sampleRate * blockAlign > 0xFFFFFFFF) {
        rc = ERR_BAD_ARGS;
        goto fail;
    }

    hr = CoCreateInstance(&CLSID_MMDeviceEnumerator_IID, NULL, CLSCTX_ALL,
                          &IID_IMMDeviceEnumerator_IID, (void**)&enumr);
    if (FAILED(hr)) { rc = ERR_NO_DEVICE; goto fail; }
    hr = enumr->lpVtbl->GetDefaultAudioEndpoint(enumr, eRender, eConsole, &dev);
    if (FAILED(hr)) { rc = ERR_NO_DEVICE; goto fail; }
    hr = dev->lpVtbl->Activate(dev, &IID_IAudioClient_IID, CLSCTX_ALL,
                               NULL, (void**)&s->client);
    if (FAILED(hr)) { rc = ERR_ACTIVATE; goto fail; }

    // Format PCM entier requis en mode exclusif : pas de float32.
    s->fmt = (WAVEFORMATEX*)HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, sizeof(WAVEFORMATEX));
    if (!s->fmt) {
        rc = ERR_NO_DEVICE;
        goto fail;
    }
    s->fmt->wFormatTag      = WAVE_FORMAT_PCM;
    s->fmt->nChannels       = (WORD)channels;
    s->fmt->nSamplesPerSec  = (DWORD)sampleRate;
    s->fmt->wBitsPerSample  = (WORD)bitsPerSample;
    s->fmt->nBlockAlign     = (WORD)blockAlign;
    s->fmt->nAvgBytesPerSec = (DWORD)sampleRate * s->fmt->nBlockAlign;

    dur = (REFERENCE_TIME)(bufferMs) * 10000;

    // --- Le point cle : AUDCLNT_SHAREMODE_EXCLUSIVE ---
    hr = s->client->lpVtbl->Initialize(s->client,
                                       AUDCLNT_SHAREMODE_EXCLUSIVE,
                                       0,              // pas de flag evenement : mode polling
                                       dur, dur,
                                       s->fmt, NULL);
    if (FAILED(hr)) { rc = (hr == AUDCLNT_E_UNSUPPORTED_FORMAT) ? ERR_FORMAT : ERR_INIT; goto fail; }

    hr = s->client->lpVtbl->GetBufferSize(s->client, &bufferSize);
    if (FAILED(hr) || bufferSize == 0) { rc = ERR_INIT; goto fail; }
    s->framesPerBuf = bufferSize;

    hr = s->client->lpVtbl->GetService(s->client, &IID_IAudioRenderClientLocal, (void**)&s->render);
    if (FAILED(hr)) { rc = ERR_GETSERVICE; goto fail; }

    s->frameSize = s->fmt->nBlockAlign;

    hr = s->client->lpVtbl->Start(s->client);
    if (FAILED(hr)) { rc = ERR_STARTRUN; goto fail; }

    if (dev) { dev->lpVtbl->Release(dev); dev = NULL; }
    if (enumr) { enumr->lpVtbl->Release(enumr); enumr = NULL; }
    *outSession = s;
    com_leave(needComUninitialize);
    return ERR_OK;

fail:
    if (s->render) s->render->lpVtbl->Release(s->render);
    if (s->client) s->client->lpVtbl->Release(s->client);
    if (dev) dev->lpVtbl->Release(dev);
    if (enumr) enumr->lpVtbl->Release(enumr);
    HeapFree(GetProcessHeap(), 0, s->fmt);
    HeapFree(GetProcessHeap(), 0, s);
    com_leave(needComUninitialize);
    return rc;
}

// ---------------------------------------------------------------------------
// Espace libre courant, en frames.
// ---------------------------------------------------------------------------
__declspec(dllexport)
int32_t __cdecl wasapi_get_free_space(void* session, int32_t* pFramesFree)
{
    WasapiSession* s = (WasapiSession*)session;
    UINT32 padding = 0, cap = 0;
    HRESULT hr;
    BOOL needComUninitialize = FALSE;
    int32_t rc;

    if (!s || !s->client || !pFramesFree) return ERR_BAD_HANDLE;
    if (!com_enter(&needComUninitialize)) return ERR_COINIT;

    hr = s->client->lpVtbl->GetCurrentPadding(s->client, &padding);
    if (FAILED(hr)) {
        rc = (int32_t)hr;
        com_leave(needComUninitialize);
        return rc;
    }
    cap = s->framesPerBuf;
    *pFramesFree = (int32_t)(cap > padding ? (cap - padding) : 0);
    com_leave(needComUninitialize);
    return ERR_OK;
}

// ---------------------------------------------------------------------------
// Ecriture bloquante : 'data' contient dataLenFrames frames entrelacees au
// format du flux. Tourne jusqu'a ce que tout soit envoye.
// ---------------------------------------------------------------------------
__declspec(dllexport)
int32_t __cdecl wasapi_write(void* session, const void* data, int32_t dataLenFrames)
{
    WasapiSession* s = (WasapiSession*)session;
    const BYTE* src = (const BYTE*)data;
    UINT32 remaining = (UINT32)dataLenFrames;
    UINT32 cap;
    BOOL needComUninitialize = FALSE;
    int32_t rc;

    if (!s || !s->render || !data || dataLenFrames < 0) return ERR_BAD_HANDLE;
    if (!com_enter(&needComUninitialize)) return ERR_COINIT;

    cap = s->framesPerBuf;

    while (remaining > 0) {
        UINT32 padding = 0, free_ = 0, chunk, bufFrames;
        BYTE* dst = NULL;
        HRESULT hr;

        hr = s->client->lpVtbl->GetCurrentPadding(s->client, &padding);
        if (FAILED(hr)) {
            rc = (int32_t)hr;
            com_leave(needComUninitialize);
            return rc;
        }

        if (padding >= cap) { Sleep(1); continue; }
        free_ = cap - padding;
        chunk = remaining < free_ ? remaining : free_;

        hr = s->render->lpVtbl->GetBuffer(s->render, chunk, &dst);
        if (FAILED(hr)) {
            rc = (int32_t)hr;
            com_leave(needComUninitialize);
            return rc;
        }

        memcpy(dst, src, (size_t)chunk * s->frameSize);
        src += (size_t)chunk * s->frameSize;
        remaining -= chunk;

        hr = s->render->lpVtbl->ReleaseBuffer(s->render, chunk, 0);
        if (FAILED(hr)) {
            rc = (int32_t)hr;
            com_leave(needComUninitialize);
            return rc;
        }
    }
    com_leave(needComUninitialize);
    return ERR_OK;
}

// ---------------------------------------------------------------------------
// Fermeture. handle invalide apres retour.
// ---------------------------------------------------------------------------
__declspec(dllexport)
int32_t __cdecl wasapi_close(void* session)
{
    WasapiSession* s = (WasapiSession*)session;
    BOOL needComUninitialize = FALSE;
    if (!s) return ERR_BAD_HANDLE;
    if (!com_enter(&needComUninitialize)) return ERR_COINIT;
    if (s->client) s->client->lpVtbl->Stop(s->client);
    if (s->render) s->render->lpVtbl->Release(s->render);
    if (s->client) s->client->lpVtbl->Release(s->client);
    HeapFree(GetProcessHeap(), 0, s->fmt);
    HeapFree(GetProcessHeap(), 0, s);
    com_leave(needComUninitialize);
    return ERR_OK;
}

__declspec(dllexport)
int32_t __cdecl wasapi_beep(int32_t frequencyHz, int32_t durationMs,
                            double amplitude)
{
    const int32_t sampleRate = 48000;
    int32_t totalFrames;

    if (InterlockedCompareExchange(&g_beepInitialized, 0, 0) == 0)
        return ERR_BAD_HANDLE;
    if (frequencyHz <= 0 || frequencyHz >= sampleRate / 2 ||
        durationMs <= 0 || durationMs > 60000 ||
        !(amplitude >= 0.0 && amplitude <= 1.0))
        return ERR_BAD_ARGS;

    LONG status = InterlockedCompareExchange(&g_beepResult, 0, 0);
    if (status != ERR_OK) return status;
    if (InterlockedCompareExchange(&g_beepMode, 3, 0) != 0)
        return ERR_BAD_ARGS;

    totalFrames = sampleRate * durationMs / 1000;
    ResetEvent(g_beepDoneEvent);
    InterlockedExchange(&g_beepAmplitude, (LONG)(amplitude * 1000000.0 + 0.5));
    InterlockedExchange(&g_beepFrequency, frequencyHz);
    InterlockedExchange(&g_beepRemainingFrames, totalFrames);
    if (InterlockedCompareExchange(&g_beepMode, 2, 3) != 3) {
        SetEvent(g_beepDoneEvent);
        return ERR_BAD_ARGS;
    }

    DWORD waitResult = WaitForSingleObject(g_beepDoneEvent, (DWORD)durationMs + 5000);
    if (waitResult != WAIT_OBJECT_0) {
        InterlockedExchange(&g_beepMode, 0);
        InterlockedExchange(&g_beepFrequency, 0);
        InterlockedExchange(&g_beepRemainingFrames, 0);
        return ERR_WRITE;
    }

    return InterlockedCompareExchange(&g_beepResult, 0, 0);
}

static DWORD WINAPI wasapi_beep_worker(LPVOID unused)
{
    const int32_t sampleRate = 48000;
    const int32_t channels = 2;
    const double pi = 3.14159265358979323846;
    BOOL needComUninitialize = FALSE;
    WasapiSession* session = NULL;
    int16_t* samples = NULL;
    int32_t rc;
    int32_t previousFrequency = 0;
    LONG previousAmplitude = 0;
    LONG previousMode = 0;
    int32_t rampFrames = 0;
    double amplitude = 0.0;
    double phase = 0.0;

    (void)unused;

    if (!com_enter(&needComUninitialize)) {
        InterlockedExchange(&g_beepResult, ERR_COINIT);
        SetEvent(g_beepReadyEvent);
        return 0;
    }

    rc = wasapi_open((void**)&session, sampleRate, channels, 16, 10);
    if (rc != ERR_OK) {
        InterlockedExchange(&g_beepResult, rc);
        SetEvent(g_beepReadyEvent);
        com_leave(needComUninitialize);
        return 0;
    }

    samples = (int16_t*)HeapAlloc(
        GetProcessHeap(), 0,
        (size_t)session->framesPerBuf * channels * sizeof(int16_t));
    if (!samples) {
        InterlockedExchange(&g_beepResult, ERR_NO_DEVICE);
        SetEvent(g_beepReadyEvent);
        wasapi_close(session);
        com_leave(needComUninitialize);
        return 0;
    }

    InterlockedExchange(&g_beepResult, ERR_OK);
    SetEvent(g_beepReadyEvent);

    while (InterlockedCompareExchange(&g_beepStop, 0, 0) == 0) {
        LONG mode = InterlockedCompareExchange(&g_beepMode, 0, 0);
        int32_t frequency = InterlockedCompareExchange(&g_beepFrequency, 0, 0);
        LONG amplitudeValue = InterlockedCompareExchange(&g_beepAmplitude, 0, 0);
        LONG remainingFrames = InterlockedCompareExchange(
            &g_beepRemainingFrames, 0, 0);
        UINT32 toneFrames = session->framesPerBuf;
        BOOL finalChunk = mode == 2 &&
            remainingFrames <= (LONG)session->framesPerBuf;
        double targetAmplitude = (double)amplitudeValue / 1000000.0;

        if (mode != 1 && mode != 2) {
            toneFrames = 0;
        } else if (mode == 2 && remainingFrames < (LONG)toneFrames) {
            toneFrames = (UINT32)remainingFrames;
        }

        if (mode != previousMode || frequency != previousFrequency ||
            amplitudeValue != previousAmplitude) {
            previousMode = mode;
            previousFrequency = frequency;
            previousAmplitude = amplitudeValue;
            rampFrames = sampleRate / 200;
        }

        for (UINT32 frame = 0; frame < session->framesPerBuf; ++frame) {
            int16_t value;
            double frameTarget = frame < toneFrames ? targetAmplitude : 0.0;

            if (finalChunk && frame < toneFrames &&
                toneFrames - frame <= sampleRate / 200)
                frameTarget *= (double)(toneFrames - frame) / (sampleRate / 200);

            if (rampFrames > 0) {
                amplitude += (frameTarget - amplitude) / rampFrames;
                --rampFrames;
            } else {
                amplitude = frameTarget;
            }

            value = (int16_t)(sin(phase) * 32767.0 * amplitude);
            samples[frame * channels] = value;
            samples[frame * channels + 1] = value;

            if (frame < toneFrames && frequency) {
                phase += 2.0 * pi * frequency / sampleRate;
                if (phase >= 2.0 * pi) phase -= 2.0 * pi;
            }
        }

        rc = wasapi_write(session, samples, (int32_t)session->framesPerBuf);
        if (rc != ERR_OK) {
            InterlockedExchange(&g_beepResult, rc);
            SetEvent(g_beepDoneEvent);
            break;
        }

        if (mode == 2) {
            LONG remaining = remainingFrames - (LONG)toneFrames;
            if (remaining < 0) remaining = 0;
            InterlockedExchange(&g_beepRemainingFrames, remaining);
            if (remaining == 0) {
                if (InterlockedCompareExchange(&g_beepMode, 0, 2) == 2) {
                    InterlockedExchange(&g_beepFrequency, 0);
                    SetEvent(g_beepDoneEvent);
                }
            }
        }
    }

    HeapFree(GetProcessHeap(), 0, samples);
    rc = wasapi_close(session);
    if (rc != ERR_OK &&
        InterlockedCompareExchange(&g_beepResult, 0, 0) == ERR_OK)
        InterlockedExchange(&g_beepResult, rc);
    com_leave(needComUninitialize);
    return 0;
}

__declspec(dllexport)
int32_t __cdecl wasapi_beep_init(void)
{
    DWORD waitResult;
    LONG status;

    if (InterlockedCompareExchange(&g_beepInitialized, 1, 0) != 0)
        return ERR_BAD_ARGS;

    InterlockedExchange(&g_beepFrequency, 0);
    InterlockedExchange(&g_beepAmplitude, 0);
    InterlockedExchange(&g_beepMode, 0);
    InterlockedExchange(&g_beepRemainingFrames, 0);
    InterlockedExchange(&g_beepStop, 0);
    InterlockedExchange(&g_beepResult, ERR_OK);
    g_beepReadyEvent = CreateEventW(NULL, TRUE, FALSE, NULL);
    if (!g_beepReadyEvent) {
        InterlockedExchange(&g_beepInitialized, 0);
        return ERR_INIT;
    }
    g_beepDoneEvent = CreateEventW(NULL, FALSE, FALSE, NULL);
    if (!g_beepDoneEvent) {
        CloseHandle(g_beepReadyEvent);
        g_beepReadyEvent = NULL;
        InterlockedExchange(&g_beepInitialized, 0);
        return ERR_INIT;
    }

    g_beepThread = CreateThread(
        NULL, 0, wasapi_beep_worker, NULL, 0, NULL);
    if (!g_beepThread) {
        CloseHandle(g_beepDoneEvent);
        CloseHandle(g_beepReadyEvent);
        g_beepDoneEvent = NULL;
        g_beepReadyEvent = NULL;
        InterlockedExchange(&g_beepInitialized, 0);
        return ERR_INIT;
    }

    waitResult = WaitForSingleObject(g_beepReadyEvent, 5000);
    status = InterlockedCompareExchange(&g_beepResult, 0, 0);
    if (waitResult != WAIT_OBJECT_0 || status != ERR_OK) {
        InterlockedExchange(&g_beepStop, 1);
        WaitForSingleObject(g_beepThread, INFINITE);
        CloseHandle(g_beepThread);
        CloseHandle(g_beepDoneEvent);
        CloseHandle(g_beepReadyEvent);
        g_beepThread = NULL;
        g_beepDoneEvent = NULL;
        g_beepReadyEvent = NULL;
        InterlockedExchange(&g_beepInitialized, 0);
        return waitResult == WAIT_OBJECT_0 ? status : ERR_INIT;
    }

    return ERR_OK;
}

__declspec(dllexport)
int32_t __cdecl wasapi_beep_on(int32_t frequencyHz, double amplitude)
{
    if (InterlockedCompareExchange(&g_beepInitialized, 0, 0) == 0)
        return ERR_BAD_HANDLE;
    if (frequencyHz <= 0 || frequencyHz >= 24000 ||
        !(amplitude >= 0.0 && amplitude <= 1.0))
        return ERR_BAD_ARGS;

    LONG status = InterlockedCompareExchange(&g_beepResult, 0, 0);
    if (status != ERR_OK) return status;
    if (InterlockedCompareExchange(&g_beepMode, 0, 0) == 3)
        return ERR_BAD_ARGS;
    InterlockedExchange(&g_beepAmplitude, (LONG)(amplitude * 1000000.0 + 0.5));
    InterlockedExchange(&g_beepFrequency, frequencyHz);
    InterlockedExchange(&g_beepRemainingFrames, 0);
    LONG previousMode = InterlockedExchange(&g_beepMode, 1);
    if (previousMode == 2) SetEvent(g_beepDoneEvent);
    return ERR_OK;
}

__declspec(dllexport)
int32_t __cdecl wasapi_beep_off(void)
{
    if (InterlockedCompareExchange(&g_beepInitialized, 0, 0) == 0)
        return ERR_BAD_HANDLE;

    LONG status = InterlockedCompareExchange(&g_beepResult, 0, 0);
    if (status != ERR_OK) return status;
    InterlockedExchange(&g_beepMode, 0);
    InterlockedExchange(&g_beepFrequency, 0);
    InterlockedExchange(&g_beepRemainingFrames, 0);
    SetEvent(g_beepDoneEvent);
    return ERR_OK;
}

__declspec(dllexport)
int32_t __cdecl wasapi_beep_close(void)
{
    LONG status;

    if (InterlockedCompareExchange(&g_beepInitialized, 0, 0) == 0)
        return ERR_BAD_HANDLE;

    InterlockedExchange(&g_beepStop, 1);
    InterlockedExchange(&g_beepMode, 0);
    InterlockedExchange(&g_beepFrequency, 0);
    SetEvent(g_beepDoneEvent);
    WaitForSingleObject(g_beepThread, INFINITE);
    status = InterlockedCompareExchange(&g_beepResult, 0, 0);
    CloseHandle(g_beepThread);
    CloseHandle(g_beepDoneEvent);
    CloseHandle(g_beepReadyEvent);
    g_beepThread = NULL;
    g_beepDoneEvent = NULL;
    g_beepReadyEvent = NULL;
    InterlockedExchange(&g_beepInitialized, 0);
    return status;
}

BOOL WINAPI DllMain(HINSTANCE h, DWORD r, LPVOID r2)
{ (void)h; (void)r2; return (r == DLL_PROCESS_ATTACH || r == DLL_PROCESS_DETACH) ? TRUE : FALSE; }