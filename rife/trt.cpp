#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <string>
#include <thread>
#include <vector>

#if _WIN32
#define NOMINMAX
#include <windows.h>
#define EXPORT extern "C" __declspec(dllexport)
#else
#include <dlfcn.h>
#define EXPORT extern "C" __attribute__((visibility("default")))
#endif

#include "NvInfer.h"
#include "NvOnnxParser.h"

using namespace nvinfer1;

namespace {

struct Logger : ILogger {
    void log(Severity severity, const char* message) noexcept override
    {
        if (severity <= Severity::kERROR)
            fprintf(stderr, "tensorrt: %s\n", message);
    }
} logger;

// tensorrt and cuda are loaded at run time, so the build needs the headers only
void* (*create_builder)(void*, int32_t);
void* (*create_runtime)(void*, int32_t);
void* (*create_parser)(void*, void*, int);
int (*cuda_malloc)(void**, size_t);
int (*cuda_free)(void*);
int (*cuda_malloc_host)(void**, size_t);
int (*cuda_free_host)(void*);
int (*cuda_memcpy)(void*, const void*, size_t, int);

const int TO_DEVICE = 1, TO_HOST = 2, ON_DEVICE = 3;

#if _WIN32
const wchar_t* const CUDA = L"cudart64_12.dll";
const wchar_t* const INFER = L"nvinfer_10.dll";
const wchar_t* const ONNX = L"nvonnxparser_10.dll";

// the folder of this library
std::filesystem::path here()
{
    HMODULE module = nullptr;
    wchar_t name[32768];
    const DWORD flags = GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
    if (!GetModuleHandleExW(flags, (LPCWSTR)&here, &module) || !GetModuleFileNameW(module, name, 32768))
        return {};
    return std::filesystem::path(name).parent_path();
}

void* open(const std::filesystem::path& file)
{
    // the library finds the libraries that it needs in its own folder
    return LoadLibraryExW(file.c_str(), nullptr, LOAD_WITH_ALTERED_SEARCH_PATH);
}

void* symbol(void* library, const char* name)
{
    return (void*)GetProcAddress((HMODULE)library, name);
}
#else
const char* const CUDA = "libcudart.so.12";
const char* const INFER = "libnvinfer.so.10";
const char* const ONNX = "libnvonnxparser.so.10";

std::filesystem::path here()
{
    Dl_info info;
    if (!dladdr((void*)&here, &info))
        return {};
    return std::filesystem::path(info.dli_fname).parent_path();
}

void* open(const std::filesystem::path& file)
{
    return dlopen(file.c_str(), RTLD_NOW | RTLD_GLOBAL);
}

void* symbol(void* library, const char* name)
{
    return dlsym(library, name);
}
#endif

bool load_libraries()
{
    static int state = 0;
    if (state != 0)
        return state > 0;
    state = -1;
    const std::filesystem::path folder = here() / "tensorrt";
    void* cuda = open(folder / CUDA);
    void* infer = open(folder / INFER);
    void* onnx = open(folder / ONNX);
    if (!cuda || !infer || !onnx)
        return false;
    create_builder = (decltype(create_builder))symbol(infer, "createInferBuilder_INTERNAL");
    create_runtime = (decltype(create_runtime))symbol(infer, "createInferRuntime_INTERNAL");
    create_parser = (decltype(create_parser))symbol(onnx, "createNvOnnxParser_INTERNAL");
    cuda_malloc = (decltype(cuda_malloc))symbol(cuda, "cudaMalloc");
    cuda_free = (decltype(cuda_free))symbol(cuda, "cudaFree");
    cuda_malloc_host = (decltype(cuda_malloc_host))symbol(cuda, "cudaMallocHost");
    cuda_free_host = (decltype(cuda_free_host))symbol(cuda, "cudaFreeHost");
    cuda_memcpy = (decltype(cuda_memcpy))symbol(cuda, "cudaMemcpy");
    if (create_builder && create_runtime && create_parser && cuda_malloc && cuda_free && cuda_malloc_host && cuda_free_host && cuda_memcpy)
        state = 1;
    return state > 0;
}

template <typename Rows>
void bands(int rows, Rows work)
{
    const int count = std::clamp((int)std::thread::hardware_concurrency(), 1, 8);
    std::vector<std::thread> threads;
    for (int band = 0; band < count; band++)
        threads.emplace_back(work, rows * band / count, rows * (band + 1) / count);
    for (std::thread& thread : threads)
        thread.join();
}

// tells how far the build of an engine is, from the steps of its first phase
struct Monitor : IProgressMonitor {
    void (*report)(void*, float) = nullptr;
    void* user = nullptr;
    std::string phase;
    int steps = 0;

    void phaseStart(const char* name, const char* parent, int32_t count) noexcept override
    {
        if (!parent && phase.empty()) {
            phase = name;
            steps = count;
        }
    }

    bool stepComplete(const char* name, int32_t step) noexcept override
    {
        if (report && phase == name && steps > 0)
            report(user, (float)(step + 1) / steps);
        return true;
    }

    void phaseFinish(const char*) noexcept override { }
};

struct Rife {
    std::filesystem::path folder;
    Monitor monitor;
    IRuntime* runtime = nullptr;
    ICudaEngine* engine = nullptr;
    IExecutionContext* context = nullptr;
    int width = 0, height = 0;
    // the size that the model takes, a multiple of 64
    int wide = 0, high = 0;
    // two rgb frames and the time, and in older models 4 more planes that depend on the size only
    int channels = 0;
    float* input = nullptr;
    float* output = nullptr;
    // the source frames that are on the gpu now
    std::vector<uint8_t> held[2];
    float time = -1;
    // page-locked memory, which moves to and from the gpu faster
    float* planes = nullptr;

    ~Rife()
    {
        delete context;
        delete engine;
        delete runtime;
        if (input)
            cuda_free(input);
        if (output)
            cuda_free(output);
        if (planes)
            cuda_free_host(planes);
    }

    static std::vector<char> read(const std::filesystem::path& file)
    {
        std::ifstream stream(file, std::ios::binary);
        return { std::istreambuf_iterator<char>(stream), std::istreambuf_iterator<char>() };
    }

    std::vector<char> build(const std::filesystem::path& name)
    {
        std::vector<char> plan;
        IBuilder* builder = (IBuilder*)create_builder(&logger, NV_TENSORRT_VERSION);
        if (!builder)
            return plan;
        INetworkDefinition* network = builder->createNetworkV2(0);
        nvonnxparser::IParser* parser = (nvonnxparser::IParser*)create_parser(network, &logger, NV_ONNX_PARSER_VERSION);
        IBuilderConfig* config = builder->createBuilderConfig();
        // the parser gets the bytes, it cannot open a path with letters outside of ascii on windows
        const std::vector<char> model = read(folder / "rife.onnx");
        if (parser && parser->parse(model.data(), model.size())) {
            const Dims4 size(1, network->getInput(0)->getDimensions().d[1], high, wide);
            IOptimizationProfile* profile = builder->createOptimizationProfile();
            for (OptProfileSelector kind : { OptProfileSelector::kMIN, OptProfileSelector::kOPT, OptProfileSelector::kMAX })
                profile->setDimensions(network->getInput(0)->getName(), kind, size);
            config->addOptimizationProfile(profile);
            config->setFlag(BuilderFlag::kFP16);
            config->setProgressMonitor(&monitor);
            // what the builder learned about the gpu in other builds makes this one shorter
            const std::filesystem::path learned = folder.parent_path() / "timing.cache";
            const std::vector<char> known = read(learned);
            ITimingCache* cache = config->createTimingCache(known.data(), known.size());
            config->setTimingCache(*cache, true);
            if (IHostMemory* memory = builder->buildSerializedNetwork(*network, *config)) {
                plan.assign((const char*)memory->data(), (const char*)memory->data() + memory->size());
                delete memory;
                // the app can build the same engine two times at once, so the file appears complete
                std::filesystem::path part = name;
                part += ".part";
                std::ofstream(part, std::ios::binary).write(plan.data(), (std::streamsize)plan.size());
                std::error_code failed;
                std::filesystem::rename(part, name, failed);
                if (IHostMemory* timing = cache->serialize()) {
                    std::ofstream(learned, std::ios::binary).write((const char*)timing->data(), (std::streamsize)timing->size());
                    delete timing;
                }
            }
            delete cache;
        }
        delete config;
        delete parser;
        delete network;
        delete builder;
        return plan;
    }

    bool prepare(int w, int h)
    {
        width = w;
        height = h;
        wide = (w + 63) / 64 * 64;
        high = (h + 63) / 64 * 64;
        // an engine fits one size, one gpu model and one tensorrt version
        const std::filesystem::path name = folder / (std::to_string(wide) + "x" + std::to_string(high) + ".engine");
        std::vector<char> plan = read(name);
        runtime = (IRuntime*)create_runtime(&logger, NV_TENSORRT_VERSION);
        if (!runtime)
            return false;
        if (!plan.empty())
            engine = runtime->deserializeCudaEngine(plan.data(), plan.size());
        if (!engine) {
            plan = build(name);
            if (plan.empty())
                return false;
            engine = runtime->deserializeCudaEngine(plan.data(), plan.size());
        }
        if (!engine || !(context = engine->createExecutionContext()))
            return false;

        const size_t pixels = (size_t)wide * high;
        const char* in = engine->getIOTensorName(0);
        const char* out = engine->getIOTensorName(1);
        channels = engine->getTensorShape(in).d[1];
        if (cuda_malloc((void**)&input, channels * pixels * sizeof(float)) || cuda_malloc((void**)&output, 3 * pixels * sizeof(float)))
            return false;
        if (!context->setInputShape(in, Dims4(1, channels, high, wide)) || !context->setTensorAddress(in, input) || !context->setTensorAddress(out, output))
            return false;

        if (cuda_malloc_host((void**)&planes, 4 * pixels * sizeof(float)))
            return false;
        for (int y = 0; y < high; y++) {
            for (int x = 0; x < wide; x++) {
                const size_t at = (size_t)y * wide + x;
                planes[at] = 2.f * x / (wide - 1) - 1;
                planes[pixels + at] = 2.f * y / (high - 1) - 1;
                planes[2 * pixels + at] = 2.f / (wide - 1);
                planes[3 * pixels + at] = 2.f / (high - 1);
            }
        }
        held[0].assign((size_t)w * h * 3, 0);
        held[1].assign((size_t)w * h * 3, 0);
        // the two frame slots start black, like the frames that they are compared with
        std::vector<float> black(7 * pixels, 0.f);
        return cuda_memcpy(input, black.data(), 7 * pixels * sizeof(float), TO_DEVICE) == 0
            && (channels == 7 || cuda_memcpy(input + 7 * pixels, planes, 4 * pixels * sizeof(float), TO_DEVICE) == 0);
    }

    // one source frame goes to the gpu once, the edge pixels fill the padding
    bool upload(int slot, const uint8_t* frame)
    {
        const size_t pixels = (size_t)wide * high;
        const size_t size = held[slot].size();
        if (memcmp(held[slot].data(), frame, size) == 0)
            return true;
        if (slot == 0 && memcmp(held[1].data(), frame, size) == 0) {
            // the clip moved on by one frame, so the second frame is the first one now
            if (cuda_memcpy(input, input + 3 * pixels, 3 * pixels * sizeof(float), ON_DEVICE))
                return false;
            held[0] = held[1];
            return true;
        }
        bands(high, [&](int from, int to) {
            for (int y = from; y < to; y++) {
                const uint8_t* line = frame + (size_t)std::min(y, height - 1) * width * 3;
                float* row = planes + (size_t)y * wide;
                for (int x = 0; x < wide; x++) {
                    const uint8_t* pixel = line + std::min(x, width - 1) * 3;
                    row[x] = pixel[0] / 255.f;
                    row[pixels + x] = pixel[1] / 255.f;
                    row[2 * pixels + x] = pixel[2] / 255.f;
                }
            }
        });
        memcpy(held[slot].data(), frame, size);
        return cuda_memcpy(input + slot * 3 * pixels, planes, 3 * pixels * sizeof(float), TO_DEVICE) == 0;
    }

    int process(const uint8_t* first, const uint8_t* second, int w, int h, float at, uint8_t* out)
    {
        if (!context && !prepare(w, h))
            return 1;
        if (w != width || h != height)
            return 1;
        const size_t pixels = (size_t)wide * high;
        if (!upload(0, first) || !upload(1, second))
            return 1;
        if (at != time) {
            std::fill_n(planes, pixels, at);
            if (cuda_memcpy(input + 6 * pixels, planes, pixels * sizeof(float), TO_DEVICE))
                return 1;
            time = at;
        }
        // the default stream, so the copy that follows waits for the result
        if (!context->enqueueV3(nullptr))
            return 1;
        if (cuda_memcpy(planes, output, 3 * pixels * sizeof(float), TO_HOST))
            return 1;
        bands(height, [&](int from, int to) {
            for (int y = from; y < to; y++) {
                const float* row = planes + (size_t)y * wide;
                uint8_t* line = out + (size_t)y * width * 3;
                for (int x = 0; x < width; x++) {
                    for (int plane = 0; plane < 3; plane++)
                        line[x * 3 + plane] = (uint8_t)(std::clamp(row[plane * pixels + x], 0.f, 1.f) * 255.f + 0.5f);
                }
            }
        });
        return 0;
    }
};

}

EXPORT void* rife_create(const char* models)
{
    if (!load_libraries())
        return nullptr;
    Rife* rife = new Rife;
    rife->folder = std::filesystem::u8path(models);
    return rife;
}

// loads the engine for a size or builds it, and tells how far the build is
EXPORT int rife_prepare(void* rife, int width, int height, void (*report)(void*, float), void* user)
{
    Rife* self = (Rife*)rife;
    self->monitor.report = report;
    self->monitor.user = user;
    const bool ready = self->prepare(width, height);
    self->monitor.report = nullptr;
    return ready ? 0 : 1;
}

EXPORT int rife_process(void* rife, const uint8_t* first, const uint8_t* second, int width, int height, float time, uint8_t* output)
{
    return ((Rife*)rife)->process(first, second, width, height, time, output);
}

EXPORT void rife_destroy(void* rife)
{
    delete (Rife*)rife;
}
