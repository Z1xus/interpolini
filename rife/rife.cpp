#include <cstdint>
#include <string>

#include "gpu.h"
#include "rife.h"

#if _WIN32
#include <windows.h>
#define EXPORT extern "C" __declspec(dllexport)
#else
#define EXPORT extern "C" __attribute__((visibility("default")))
#endif

EXPORT void* rife_create(const char* models)
{
    ncnn::create_gpu_instance();
    if (ncnn::get_gpu_count() == 0)
        return nullptr;

    // the newer models take the picture in larger steps of size
    const std::string name(models);
    const int padding = name.find("rife-v4.25-lite") != std::string::npos ? 128
        : name.find("rife-v4.25") != std::string::npos || name.find("rife-v4.26") != std::string::npos ? 64
        : 32;
    RIFE* rife = new RIFE(ncnn::get_default_gpu_index(), false, false, false, 1, false, true, padding);
#if _WIN32
    std::wstring path(MultiByteToWideChar(CP_UTF8, 0, models, -1, nullptr, 0), 0);
    MultiByteToWideChar(CP_UTF8, 0, models, -1, path.data(), (int)path.size());
    path.resize(path.size() - 1);
#else
    const std::string& path = name;
#endif
    if (rife->load(path) != 0)
    {
        delete rife;
        return nullptr;
    }
    return rife;
}

// the engine of ncnn needs no build, the call is here so both plugins have the same set
EXPORT int rife_prepare(void*, int, int, void (*)(void*, float), void*)
{
    return 0;
}

EXPORT int rife_process(void* rife, const uint8_t* first, const uint8_t* second, int width, int height, float time, uint8_t* output)
{
    const ncnn::Mat a(width, height, (void*)first, (size_t)3, 3);
    const ncnn::Mat b(width, height, (void*)second, (size_t)3, 3);
    ncnn::Mat out(width, height, (void*)output, (size_t)3, 3);
    return ((RIFE*)rife)->process_v4(a, b, time, out);
}

EXPORT void rife_destroy(void* rife)
{
    delete (RIFE*)rife;
}
