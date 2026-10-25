// Original FilmCraft SDK adapter. Requires locally supplied RED headers/libraries.
// No SDK source or headers are redistributed here. See docs/r3d-ne.md.
// A separate process contains native faults in the vendor decoder.
#include <R3DSDK.h>
#include <R3DSDKDecoder.h>
#ifdef _WIN32
#include <windows.h>
#include <fcntl.h>
#include <io.h>
#include <malloc.h>
#else
#include <cerrno>
#include <csignal>
#include <fcntl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>
#endif
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <iostream>
#include <memory>
#include <sstream>
#include <iomanip>
#include <stdexcept>
#include <string>
#include <condition_variable>
#include <mutex>
#include <cstdlib>
#include <chrono>
#include <array>
#include <deque>
#include <map>
#include <thread>
#include <vector>

namespace {
constexpr size_t MAX_PIXELS = 33554432;
constexpr size_t MAX_AUDIO = 1048576;
// Video requests the helper holds while every slot is busy (the app keeps slots + 1 in flight).
constexpr size_t MAX_WAITING = 64;
// A delivered frame the app has not handed back after this long is a fatal helper error, as a
// request that takes this long is a fatal error in the app.
constexpr auto SLOT_TIMEOUT = std::chrono::seconds(120);
void require(bool ok, const char* message) { if (!ok) throw std::runtime_error(message); }
// A protocol violation on stdin (a bad `F`): the state is no longer shared, so the helper ends.
struct fatal : std::runtime_error { using std::runtime_error::runtime_error; };
// A bounded integer from the environment: unset means the default, anything else is an error.
size_t env_number(const char* name, size_t lo, size_t hi, size_t fallback) {
    const char* value = std::getenv(name);
    if (!value) return fallback;
    char* end = nullptr;
    const auto n = std::strtoull(value, &end, 10);
    if (end == value || *end != 0 || n < lo || n > hi)
        throw std::runtime_error(std::string(name) + " must be " + std::to_string(lo) + ".." + std::to_string(hi));
    return static_cast<size_t>(n);
}
std::mutex output_mutex;
// Replies go down a named pipe the helper creates (FCRD5): a 31 MB half-resolution 6K frame
// through the 4 KB stdout pipe cost ~30 ms to write and ~20 ms to read, one page-sized hop
// at a time, and a decode slot stays busy until its reply is written. Since FCRD6 frames do not
// go down the pipe at all (see `Shared`); the pipe carries headers, metadata, errors and audio.
// Until the pipe is connected (start-up failures), stdout carries the same framing, so the app
// can still read the error.
//
// On Linux stdout is the reply pipe: the app's pipe grows to 1 MiB (`F_SETPIPE_SZ`, the
// unprivileged limit), nothing frame-sized goes down it since FCRD6, and the name argument
// only names the shared frame memory.
#ifdef _WIN32
HANDLE output = INVALID_HANDLE_VALUE;
constexpr DWORD OUTPUT_BUFFER = 8u << 20;
void write_all(const void* bytes, size_t length) {
    if (output == INVALID_HANDLE_VALUE) {
        require(length == 0 || std::fwrite(bytes, 1, length, stdout) == length, "output closed");
        return;
    }
    const auto* p = static_cast<const unsigned char*>(bytes);
    while (length > 0) {
        const DWORD chunk = static_cast<DWORD>(length < (1u << 30) ? length : (1u << 30));
        DWORD written = 0;
        require(WriteFile(output, p, chunk, &written, nullptr) && written > 0 && written <= chunk, "output closed");
        p += written;
        length -= written;
    }
}
void flush_output() {
    if (output == INVALID_HANDLE_VALUE) require(std::fflush(stdout) == 0, "output closed");
}
#else
void write_all(const void* bytes, size_t length) {
    require(length == 0 || std::fwrite(bytes, 1, length, stdout) == length, "output closed");
}
void flush_output() { require(std::fflush(stdout) == 0, "output closed"); }
#endif
// FCRD6 framing: little-endian status:u32, request id:u64, length:u64, slot:u32, then the
// payload. Status 0: `length` payload bytes follow. Status 1: an error string follows. Status 2:
// no bytes follow; the frame is the first `length` bytes of shared slot `slot`. ID 0 is startup.
void reply(uint64_t id, uint32_t status, const void* bytes, uint64_t length, uint32_t slot = 0) {
    std::lock_guard<std::mutex> lock(output_mutex);
    unsigned char header[24];
    std::memcpy(header, &status, 4);
    std::memcpy(header + 4, &id, 8);
    std::memcpy(header + 12, &length, 8);
    std::memcpy(header + 20, &slot, 4);
    write_all(header, sizeof header);
    if (status != 2) write_all(bytes, static_cast<size_t>(length));
    flush_output();
}
// The name the app chose for this helper's reply pipe and shared frame memory: letters, digits
// and dashes, at most 64 (a UTF-16 argument on Windows, bytes elsewhere).
#ifdef _WIN32
using Name = std::wstring;
#else
using Name = std::string;
#endif
Name checked_name(const Name::value_type* name) {
    Name value;
    size_t n = 0;
    for (const auto* c = name; *c; ++c, ++n) {
        require(n < 64 && ((*c >= 'a' && *c <= 'z') || (*c >= 'A' && *c <= 'Z') || (*c >= '0' && *c <= '9') || *c == '-'), "invalid reply pipe name");
        value += *c;
    }
    require(n > 0, "invalid reply pipe name");
    return value;
}
#ifdef _WIN32
// The reply pipe `\\.\pipe\<name>`: one instance, no remote clients, created before anything
// else can fail, so the app connects as soon as the process is up.
void open_output(const Name& name) {
    std::wstring path = L"\\\\.\\pipe\\" + name;
    HANDLE pipe = CreateNamedPipeW(path.c_str(), PIPE_ACCESS_OUTBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
        PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS, 1, OUTPUT_BUFFER, 4096, 0, nullptr);
    require(pipe != INVALID_HANDLE_VALUE, "cannot create the reply pipe");
    if (!ConnectNamedPipe(pipe, nullptr) && GetLastError() != ERROR_PIPE_CONNECTED) {
        CloseHandle(pipe);
        throw std::runtime_error("the app did not connect to the reply pipe");
    }
    output = pipe;
}
#else
// stdout is the reply pipe; widen it (best effort: a kernel that refuses keeps 64 KiB, which
// only costs an audio reply a few more hops).
void open_output(const Name&) { fcntl(STDOUT_FILENO, F_SETPIPE_SZ, 1 << 20); }
#endif
void error(uint64_t id, const char* message) { reply(id, 1, message, std::strlen(message)); }
#ifdef _WIN32
std::string utf8(const wchar_t* input) {
    int n = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, input, -1, nullptr, 0, nullptr, nullptr);
    require(n > 0, "invalid Unicode path");
    std::string value(static_cast<size_t>(n), '\0');
    require(WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, input, -1, value.data(), n, nullptr, nullptr) == n, "invalid Unicode path");
    value.pop_back();
    return value;
}
#endif
struct Sdk {
    explicit Sdk(const char* path, bool gpu) {
        auto status = R3DSDK::InitializeSdk(path, gpu ? OPTION_RED_DECODER : OPTION_RED_NONE);
        if (status != R3DSDK::ISInitializeOK) {
            R3DSDK::FinalizeSdk();
            throw std::runtime_error("RED SDK initialization failed; check the 9.2.1 runtime libraries beside the helper or RED_R3D_LIBRARY_PATH");
        }
    }
    // SDK 9.2.1 on Linux faults inside FinalizeSdk() once the managed CUDA decoder has processed
    // a frame (deterministic; releasing the jobs, the decoder and the clip first, or waiting,
    // does not help). The helper process ends right after, so there it is left unfinalized.
    bool finalize = true;
    ~Sdk() { if (finalize) R3DSDK::FinalizeSdk(); }
};
// 512-byte aligned decode buffers (the SDK wants at least 16; sector alignment suits the
// output pipe), through each C runtime's own aligned allocator.
void* aligned_allocate(size_t bytes) {
#ifdef _WIN32
    return _aligned_malloc(bytes, 512);
#else
    void* data = nullptr;
    return posix_memalign(&data, 512, bytes) == 0 ? data : nullptr;
#endif
}
void aligned_release(void* data) {
#ifdef _WIN32
    _aligned_free(data);
#else
    std::free(data);
#endif
}
// A half-float NaN in the last pixel before each decode: a decode that writes fewer rows than
// the size rule says (a reused slot would otherwise still hold the previous frame there) is
// rejected by the app's non-finite check instead of being shown. The 16-bit integer outputs
// read it as a code value, so there it only replaces a stale pixel with white.
void sentinel(void* buffer, size_t bytes) {
    if (bytes >= 6) std::memset(static_cast<unsigned char*>(buffer) + bytes - 6, 0xFF, 6);
}
struct Aligned {
    std::unique_ptr<void, void (*)(void*)> data;
    explicit Aligned(size_t bytes) : data(aligned_allocate(bytes), &aligned_release) {
        require(data != nullptr, "decode buffer allocation failed");
    }
};
// The frame slots the app reads in place (FCRD6): a pagefile-backed section `Local\<name>-frames`
// (Windows) or a POSIX shared memory object `/<name>-frames` (Linux, a tmpfs file under
// /dev/shm) of `slots` x `slot_bytes` (the clip's full-resolution frame, rounded to pages),
// created before the metadata reply names it. The SDK decodes straight into a slot, the reply
// carries the slot index, and the app hands the slot back with `F <slot>`. Pages cost physical
// memory only once touched, so a clip played at half resolution never backs the full-resolution
// tail of a slot. The memory outlives this process for as long as the app keeps its view, so a
// helper that dies mid-frame leaves the app reading valid memory.
struct Shared {
#ifdef _WIN32
    HANDLE mapping = nullptr;
#else
    std::string path;
    size_t total = 0;
#endif
    unsigned char* base = nullptr;
    size_t slots = 0, slot_bytes = 0;
    Shared(const Name& name, size_t count, size_t bytes) : slots(count), slot_bytes((bytes + 4095) / 4096 * 4096) {
        require(count > 0 && slot_bytes > 0 && slot_bytes <= SIZE_MAX / count, "frame slots too large");
        const uint64_t total = static_cast<uint64_t>(slot_bytes) * count;
#ifdef _WIN32
        const std::wstring path = L"Local\\" + name + L"-frames";
        mapping = CreateFileMappingW(INVALID_HANDLE_VALUE, nullptr, PAGE_READWRITE, static_cast<DWORD>(total >> 32), static_cast<DWORD>(total), path.c_str());
        require(mapping != nullptr, "cannot create the shared frame memory");
        if (GetLastError() == ERROR_ALREADY_EXISTS) {
            CloseHandle(mapping);
            mapping = nullptr;
            throw std::runtime_error("the shared frame memory name is taken");
        }
        base = static_cast<unsigned char*>(MapViewOfFile(mapping, FILE_MAP_ALL_ACCESS, 0, 0, static_cast<size_t>(total)));
        if (!base) {
            CloseHandle(mapping);
            mapping = nullptr;
            throw std::runtime_error("cannot map the shared frame memory");
        }
#else
        // Created exclusively (a taken name is an error, never reused), sized once, mapped
        // shared, and never resized afterwards: the app checks the size before it maps.
        require(total <= static_cast<uint64_t>(INT64_MAX), "frame slots too large");
        path = "/" + name + "-frames";
        const int fd = shm_open(path.c_str(), O_RDWR | O_CREAT | O_EXCL | O_CLOEXEC, 0600);
        require(fd >= 0, errno == EEXIST ? "the shared frame memory name is taken" : "cannot create the shared frame memory");
        if (ftruncate(fd, static_cast<off_t>(total)) != 0) {
            close(fd);
            shm_unlink(path.c_str());
            throw std::runtime_error("cannot size the shared frame memory");
        }
        void* view = mmap(nullptr, static_cast<size_t>(total), PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        close(fd);
        if (view == MAP_FAILED) {
            shm_unlink(path.c_str());
            throw std::runtime_error("cannot map the shared frame memory");
        }
        base = static_cast<unsigned char*>(view);
        this->total = static_cast<size_t>(total);
#endif
    }
    Shared(const Shared&) = delete;
    Shared& operator=(const Shared&) = delete;
    ~Shared() {
#ifdef _WIN32
        if (base) UnmapViewOfFile(base);
        if (mapping) CloseHandle(mapping);
#else
        if (base) munmap(base, total);
        // The app removes the name once it has mapped the object; a helper that ends before
        // that (a start-up failure) removes it here, and a name already gone is fine.
        shm_unlink(path.c_str());
#endif
    }
    unsigned char* slot(size_t index) { return base + index * slot_bytes; }
};

void gpu_ok(R3DSDK::R3DStatus status, const char* operation) {
    if (status != R3DSDK::R3DStatus_Ok)
        throw std::runtime_error(std::string(operation) + " (RED GPU status " + std::to_string(status) + ")");
}
struct Gpu {
    std::unique_ptr<R3DSDK::R3DDecoder, decltype(&R3DSDK::R3DDecoder::ReleaseDecoder)> decoder{nullptr, &R3DSDK::R3DDecoder::ReleaseDecoder};
    std::string description;
    Gpu() {
        R3DSDK::R3DDecoderOptions* raw = nullptr;
        gpu_ok(R3DSDK::R3DDecoderOptions::CreateOptions(&raw), "Create RED GPU options");
        std::unique_ptr<R3DSDK::R3DDecoderOptions, decltype(&R3DSDK::R3DDecoderOptions::ReleaseOptions)> options(raw, &R3DSDK::R3DDecoderOptions::ReleaseOptions);
        require(raw != nullptr, "RED GPU options missing");
        std::vector<R3DSDK::CudaDeviceInfo> cuda;
        std::vector<R3DSDK::OpenCLDeviceInfo> opencl;
        if (R3DSDK::R3DDecoderOptions::GetCudaDeviceList(cuda) == R3DSDK::R3DStatus_Ok && !cuda.empty()) {
            gpu_ok(options->useDevice(cuda.front()), "Select RED CUDA device");
            description = "CUDA " + std::string(cuda.front().name, strnlen(cuda.front().name, sizeof(cuda.front().name)));
        } else {
            gpu_ok(R3DSDK::R3DDecoderOptions::GetOpenCLDeviceList(opencl), "Enumerate RED OpenCL devices");
            require(!opencl.empty(), "R3D NE requires a compatible CUDA or OpenCL GPU; none found");
            gpu_ok(options->useDevice(opencl.front()), "Select RED OpenCL device");
            description = "OpenCL " + std::string(opencl.front().name, strnlen(opencl.front().name, sizeof(opencl.front().name)));
        }
        // Keep the SDK default image-thread count; one image thread serializes all work.
        // Three GPU streams measured best (3–4 frames in flight: 25–27 fps at half resolution on
        // a current desktop GPU; more buys about one frame). The knobs are for bake-offs and smaller cards.
        gpu_ok(options->setGPUConcurrentFrameCount(env_number("FILMCRAFT_R3D_GPU_FRAMES", 1, 8, 3)), "Set RED GPU concurrency");
        gpu_ok(options->setMemoryPoolSize(env_number("FILMCRAFT_R3D_HOST_POOL_MB", 1024, 65536, 8192)), "Set RED host memory limit");
        gpu_ok(options->setGPUMemoryPoolSize(env_number("FILMCRAFT_R3D_GPU_POOL_MB", 1024, 65536, 6144)), "Set RED GPU memory limit");
        R3DSDK::R3DDecoder* instance = nullptr;
        gpu_ok(R3DSDK::R3DDecoder::CreateDecoder(options.get(), &instance), "Create RED GPU decoder");
        decoder.reset(instance);
        require(instance != nullptr, "RED GPU decoder missing");
    }
};

// One clip the app has opened in this helper (FCRD8: one helper serves every clip). The SDK's
// `Clip` is cheap to keep (an index, ~6 ms to open inside a running SDK against a helper process
// start of 0.2-0.8 s), so a timeline of a hundred clips costs one process and its pools, not a
// hundred. Requests hold their clip's `shared_ptr`, so closing it (`C`) while a frame is in
// flight frees it once that frame is written.
struct ClipState {
    std::unique_ptr<R3DSDK::Clip> clip;
    size_t width = 0, height = 0, channels = 0;
    uint64_t frames = 0, samples = 0;
    int64_t num = 0, den = 0, rate = 0, floating = 0, bits = 0;
    // The lens squeeze the camera recorded (its desqueeze factor, 1.33 for a 1.33x anamorphic
    // lens), or 0 when the clip records none.
    double squeeze = 0;
    // CPU decodes of one clip run one at a time (the SDK's clip object is not documented as
    // safe for concurrent decodes), and so does an RMD write (`W`); different clips decode in
    // parallel.
    std::mutex decode;
    // The clip's recorded IPP2 settings, or its RMD sidecar's when it has one (the SDK's
    // GetDefaultImageProcessingSettings); each request develops a copy (`develop`). Replaced
    // after an RMD write, so read and written under `settings_mutex` (`settings()`).
    std::mutex settings_mutex;
    R3DSDK::ImageProcessingSettings base;
    bool rmd = false;
    std::string rmd_path;
    size_t tracks = 1;
    bool lut = false;
    R3DSDK::ImageProcessingSettings settings() {
        std::lock_guard<std::mutex> lock(settings_mutex);
        return base;
    }
};
// The desqueeze factor a clip records: `pixel_aspect_ratio` (1.0 normally), else the older
// `anamorphic_mode` code; 0 when neither is there.
double recorded_squeeze(const R3DSDK::Clip& clip) {
    if (clip.MetadataExists(R3DSDK::RMD_PIXEL_ASPECT_RATIO)) {
        return clip.MetadataItemType(R3DSDK::RMD_PIXEL_ASPECT_RATIO) == R3DSDK::MetadataTypeFloat
                   ? static_cast<double>(clip.MetadataItemAsFloat(R3DSDK::RMD_PIXEL_ASPECT_RATIO))
                   : static_cast<double>(clip.MetadataItemAsInt(R3DSDK::RMD_PIXEL_ASPECT_RATIO));
    }
    if (clip.MetadataExists(R3DSDK::RMD_ANAMORPHIC_MODE) && clip.MetadataItemType(R3DSDK::RMD_ANAMORPHIC_MODE) == R3DSDK::MetadataTypeInt) {
        // 0 = none, 1 = 2x, 2 = 1.3x, 3 = 1.25x, 4 = 1.6x, 5 = 1.8x, 6 = 1.5x
        static const double modes[] = {1.0, 2.0, 1.3, 1.25, 1.6, 1.8, 1.5};
        const unsigned int mode = clip.MetadataItemAsInt(R3DSDK::RMD_ANAMORPHIC_MODE);
        if (mode < sizeof(modes) / sizeof(modes[0])) return modes[mode];
    }
    return 0;
}
// (Re)read what the clip records: its settings (the RMD sidecar's when one is present), whether
// it has a sidecar, an HDRx track or a creative LUT. `written`: an RMD was just written (the SDK's
// RmdSidecarPresent says what it found when the clip was loaded, not since; measured).
void describe(ClipState& c, bool written = false) {
    R3DSDK::ImageProcessingSettings settings;
    c.clip->GetDefaultImageProcessingSettings(settings);
    const char* path = c.clip->GetRmdPath();
    std::lock_guard<std::mutex> lock(c.settings_mutex);
    c.base = settings;
    c.rmd = written || c.clip->RmdSidecarPresent();
    c.rmd_path = path ? path : "";
    c.tracks = c.clip->VideoTrackCount();
    c.lut = c.clip->Camera3DLutPresent() || settings.Lut3D != nullptr;
}
// Open `path` as a clip of this helper, or throw why not. `slot_bytes` (GPU helpers) bounds the
// full-resolution frame the shared slots can hold.
std::shared_ptr<ClipState> open_clip(const std::string& path, size_t slot_bytes) {
    require(R3DSDK::IdentifyFile(path.c_str()) == R3DSDK::FileId_R3D_NE,
            "This backend supports Nikon ZR R3D NE only; file is unsupported, damaged, or unreadable");
    auto state = std::make_shared<ClipState>();
    state->clip = std::make_unique<R3DSDK::Clip>(path.c_str());
    auto& clip = *state->clip;
    require(clip.Status() == R3DSDK::LSClipLoaded, "RED SDK could not open the R3D NE clip");
    const size_t w = clip.Width(), h = clip.Height(), channels = clip.AudioChannelCount();
    require(w > 0 && h > 0 && w <= 8192 && h <= 8192 && w <= MAX_PIXELS / h, "unsupported R3D dimensions");
    require(channels <= 16, "unsupported R3D channel count");
    require(slot_bytes == 0 || w * h * 6 <= slot_bytes, "clip larger than the helper's frame slots");
    state->width = w;
    state->height = h;
    state->channels = channels;
    state->frames = clip.VideoFrameCount();
    state->samples = clip.AudioSampleCount();
    state->num = clip.MetadataItemAsInt(R3DSDK::RMD_FRAMERATE_NUMERATOR);
    state->den = clip.MetadataItemAsInt(R3DSDK::RMD_FRAMERATE_DENOMINATOR);
    state->rate = clip.MetadataItemAsInt(R3DSDK::RMD_SAMPLERATE);
    state->floating = clip.MetadataItemAsInt(R3DSDK::RMD_AUDIO_FORMAT);
    state->bits = clip.MetadataItemAsInt(R3DSDK::RMD_SAMPLE_SIZE);
    require(state->frames > 0 && state->num > 0 && state->den > 0, "invalid R3D timing metadata");
    require(channels == 0 || (state->rate > 0 && state->rate <= 384000 && state->floating <= 1 && (state->bits == 24 || state->bits == 32)),
            "invalid R3D audio metadata");
    describe(*state);
    state->squeeze = recorded_squeeze(clip);
    return state;
}
// The open clips by the app's handle.
std::mutex clips_mutex;
std::map<uint64_t, std::shared_ptr<ClipState>> clips;
constexpr size_t MAX_CLIPS = 4096;
std::shared_ptr<ClipState> find_clip(uint64_t handle) {
    std::lock_guard<std::mutex> lock(clips_mutex);
    const auto it = clips.find(handle);
    require(it != clips.end(), "unknown clip handle (reopen the clip)");
    return it->second;
}

// Slots own every object the SDK accesses until the callback and the reply finish, and a slot
// stays the app's until it is freed. Only this fixed ring is touched by callbacks: no pipe I/O
// or allocation there. The stdin thread never waits for a slot (the command that frees one
// arrives on stdin): video requests queue here and the pipeline thread submits them as slots
// free, between writing the replies.
struct Pipeline {
    enum class State { Free, Decoding, Delivered };
    struct Slot {
        Pipeline* owner = nullptr;
        size_t index = 0;
        State state = State::Free;
        uint64_t id = 0;
        // The clip this slot decodes: held until the reply is written, so a clip the app closes
        // (`C`) mid-frame stays alive for the SDK's callback.
        std::shared_ptr<struct ClipState> clip;
        size_t frame = 0;
        uint64_t divisor = 0;
        size_t bytes = 0;
        // This request's development settings: the job points here until its callback, so
        // requests in flight with different settings never share one structure.
        R3DSDK::ImageProcessingSettings settings;
        R3DSDK::R3DStatus status = R3DSDK::R3DStatus_ErrorProcessing;
        std::chrono::steady_clock::time_point submitted, completed_at, written_at;
        std::unique_ptr<R3DSDK::R3DDecodeJob, decltype(&R3DSDK::R3DDecoder::ReleaseDecodeJob)> job{nullptr, &R3DSDK::R3DDecoder::ReleaseDecodeJob};
    };
    struct Request {
        uint64_t id = 0;
        std::shared_ptr<struct ClipState> clip;
        size_t frame = 0;
        uint64_t divisor = 0;
        size_t bytes = 0, width = 0;
        R3DSDK::VideoDecodeMode mode = R3DSDK::DECODE_HALF_RES_PREMIUM;
        // Developed per request (`develop`): copied into the slot the request decodes in.
        R3DSDK::ImageProcessingSettings settings;
        R3DSDK::VideoPixelType pixel_type = R3DSDK::PixelType_HalfFloat_RGB_Interleaved;
    };
    Gpu& gpu;
    Shared& shared;
    std::array<Slot, 8> slots;
    std::array<size_t, 8> ready{};
    size_t count = 4, head = 0, queued = 0;
    std::deque<Request> waiting;
    bool stopping = false;
    std::chrono::steady_clock::time_point stop_requested{};
    const bool timing = std::getenv("FILMCRAFT_R3D_TIMING") != nullptr;
    std::mutex mutex;
    std::condition_variable changed;
    std::thread worker;

    Pipeline(Gpu& device, Shared& memory) : gpu(device), shared(memory) {
        count = memory.slots;
        require(count >= 1 && count <= slots.size(), "unsupported slot count");
        for (size_t i = 0; i < count; ++i) {
            auto& slot = slots[i];
            slot.owner = this;
            slot.index = i;
            R3DSDK::R3DDecodeJob* raw = nullptr;
            gpu_ok(R3DSDK::R3DDecoder::CreateDecodeJob(&raw), "Create RED GPU job");
            slot.job.reset(raw);
            require(raw != nullptr, "RED GPU job missing");
        }
        worker = std::thread([this] {
            try { run(); }
            catch (const std::exception& e) {
                // A failed pipe cannot report another reply, and a slot the app never freed
                // leaves the pipeline inconsistent. Report what can be reported and exit the
                // isolated helper without unwinding buffers SDK callbacks still reference; the
                // app fails the worker and reopens the clip.
                try { error(0, e.what()); } catch (...) {}
                std::_Exit(1);
            } catch (...) { std::_Exit(1); }
        });
    }
    ~Pipeline() {
        {
            std::lock_guard<std::mutex> lock(mutex);
            stopping = true;
            changed.notify_all();
        }
        if (worker.joinable()) worker.join();
    }
    // Five slots: the SDK keeps three or four frames in flight while the app repacks one.
    static size_t slot_count() { return env_number("FILMCRAFT_R3D_SLOTS", 1, 8, 5); }
    static void completed(R3DSDK::R3DDecodeJob* job, R3DSDK::R3DStatus status) noexcept {
        try {
            auto& slot = *static_cast<Slot*>(job->privateData);
            auto& self = *slot.owner;
            std::lock_guard<std::mutex> lock(self.mutex);
            if (self.queued >= self.count) std::_Exit(1);
            slot.status = status;
            slot.completed_at = std::chrono::steady_clock::now();
            self.ready[(self.head + self.queued) % self.count] = slot.index;
            ++self.queued;
            self.changed.notify_all();
        } catch (...) { std::_Exit(1); }
    }
    Slot* free_slot() {
        for (size_t i = 0; i < count; ++i) {
            if (slots[i].state == State::Free) return &slots[i];
        }
        return nullptr;
    }
    bool decoding() const {
        for (size_t i = 0; i < count; ++i) {
            if (slots[i].state == State::Decoding) return true;
        }
        return false;
    }
    // From the stdin thread: queue a video request (the pipeline thread submits it).
    void enqueue(const Request& request) {
        std::lock_guard<std::mutex> lock(mutex);
        require(waiting.size() < MAX_WAITING, "too many video requests outstanding");
        waiting.push_back(request);
        changed.notify_all();
    }
    // From the stdin thread: the app has finished with a delivered slot.
    void release(uint64_t index) {
        std::lock_guard<std::mutex> lock(mutex);
        if (index >= count || slots[index].state != State::Delivered) throw fatal("freed a frame slot that was not the app's");
        auto& slot = slots[index];
        slot.state = State::Free;
        if (timing)
            std::fprintf(stderr, "R3D slot %zu held %.3f ms\n", slot.index,
                std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - slot.written_at).count());
        changed.notify_all();
    }
    void run() {
        std::unique_lock<std::mutex> lock(mutex);
        for (;;) {
            changed.wait_for(lock, std::chrono::seconds(1), [&] { return queued != 0 || stopping || (!waiting.empty() && free_slot() != nullptr); });
            const auto now = std::chrono::steady_clock::now();
            for (size_t i = 0; i < count; ++i) {
                if (slots[i].state == State::Delivered && now - slots[i].written_at > SLOT_TIMEOUT)
                    throw std::runtime_error("the app held a decoded RED frame for 120 seconds");
            }
            while (queued != 0) {
                auto& slot = slots[ready[head]];
                head = (head + 1) % count;
                --queued;
                // The slot is the app's (or free again) before the reply goes out: the app's
                // `F` can arrive the moment the header lands, ahead of this thread retaking the
                // lock, and must find the slot delivered.
                const bool ok = slot.status == R3DSDK::R3DStatus_Ok;
                slot.written_at = std::chrono::steady_clock::now();
                slot.state = ok ? State::Delivered : State::Free;
                // the SDK is done with the clip; a clip closed meanwhile goes now
                auto finished = std::move(slot.clip);
                lock.unlock();
                finished.reset();
                if (ok)
                    reply(slot.id, 2, nullptr, slot.bytes, static_cast<uint32_t>(slot.index));
                else
                    error(slot.id, ("RED GPU frame processing failed (RED GPU status " + std::to_string(slot.status) + ")").c_str());
                if (timing) {
                    // Queue wait inside the SDK and its decode are not separable from here; the
                    // header write is; the app's hold of the slot is logged when it is freed.
                    const auto written = std::chrono::steady_clock::now();
                    std::fprintf(stderr, "R3D frame %zu divisor %llu submit-to-callback %.3f ms, callback-to-written %.3f ms\n", slot.frame,
                        static_cast<unsigned long long>(slot.divisor),
                        std::chrono::duration<double, std::milli>(slot.completed_at - slot.submitted).count(),
                        std::chrono::duration<double, std::milli>(written - slot.completed_at).count());
                }
                lock.lock();
            }
            while (!stopping && !waiting.empty()) {
                Slot* slot = free_slot();
                if (!slot) break;
                const Request request = waiting.front();
                waiting.pop_front();
                slot->state = State::Decoding;
                slot->id = request.id;
                slot->frame = request.frame;
                slot->divisor = request.divisor;
                slot->bytes = request.bytes;
                slot->settings = request.settings;
                slot->clip = request.clip;
                sentinel(shared.slot(slot->index), request.bytes);
                auto& job = *slot->job;
                job.clip = slot->clip->clip.get();
                job.videoTrackNo = 0;
                job.videoFrameNo = request.frame;
                job.callback = completed;
                job.privateData = slot;
                job.mode = request.mode;
                job.pixelType = request.pixel_type;
                job.outputBuffer = shared.slot(slot->index);
                job.outputBufferSize = request.bytes;
                job.bytesPerRow = request.width * 6; // informational: the SDK packs rows at floor(w / divisor) regardless
                job.imageProcessingSettings = &slot->settings;
                job.outputFrameMetadata = nullptr;
                slot->submitted = std::chrono::steady_clock::now();
                lock.unlock();
                // Submitted without the lock: a rejected submission has no callback and is
                // routed through `completed`, which takes it.
                const auto status = gpu.decoder->decode(&job);
                if (status != R3DSDK::R3DStatus_Ok) completed(&job, status);
                lock.lock();
            }
            // EOF: finish what the SDK still holds (the callbacks reference the slots and the
            // section), then let the buffers go. Frames the app still holds need nothing. A
            // decode the SDK never completes (a starved GPU: helpers of an app killed with
            // a hundred of them alive outlived it by hours) must not keep the process, its
            // memory and its CUDA context alive: after ten seconds exit without unwinding.
            if (stopping) {
                if (queued == 0 && !decoding()) return;
                if (stop_requested == std::chrono::steady_clock::time_point{}) stop_requested = now;
                if (now - stop_requested > std::chrono::seconds(10)) {
                    std::fprintf(stderr, "filmcraft-red-helper: a decode did not complete within 10 s of shutdown; exiting without it\n");
                    std::_Exit(0);
                }
            }
        }
    }
};

// The development fields of a request (FCRD9), as small integers the app sends and this helper
// range-checks (anything else is a malformed request, answered with an error, never applied):
// half 0 = premium, 1 = good (a half-resolution decode); space 0 Rec.709, 1 Rec.2020, 2 P3 D65,
// 3 REDWideGamutRGB; gamma 0 linear, 1 BT.1886, 2 sRGB, 3 Log3G10, 4 ST 2084, 5 Hybrid Log-Gamma;
// tone map 0 = as recorded, 1 None, 2 Low, 3 Medium, 4 High; rolloff 0 = as recorded, 1 None,
// 2 Hard, 3 Medium, 4 Soft, 5 Very Soft; peak 0 = as recorded, else cd/m² within the SDK's
// HdrPeakNits limits; options a flag word (1 chroma noise reduction set, 2 its value, 4 apply the
// metadata curves, 8 apply the creative LUT, 16 apply the CDL); flashing 0 = as recorded, 1 Off,
// 2 Mild, 3 Medium, 4 Strong. Then the RAW adjustments (`adjust`).
struct Develop {
    uint64_t half = 0, space = 0, gamma = 0, tonemap = 0, rolloff = 0, peak = 0, options = 0, flashing = 0;
    uint64_t flags = 0, iso = 0, kelvin = 0;
    int64_t tint = 0, exposure = 0;
};
bool read_develop(std::istringstream& in, Develop& d, bool with_half) {
    if (with_half) in >> d.half;
    in >> d.space >> d.gamma >> d.tonemap >> d.rolloff >> d.peak >> d.options >> d.flashing >> d.flags >> d.iso >> d.kelvin >> d.tint >> d.exposure;
    return !in.fail();
}
// The colour space and gamma curve go together the way the app's `RawDevelop::supports` says:
// display curves with display primaries, Log3G10 with REDWideGamutRGB, the HDR curves with
// Rec.2020, linear light with any of them.
bool supported(uint64_t space, uint64_t gamma) {
    switch (gamma) {
    case 0: return space <= 3;
    case 1: case 2: return space <= 2;
    case 3: return space == 3;
    case 4: case 5: return space == 1;
    default: return false;
    }
}
const float IDENTITY_CURVE[10] = {0.0f, 0.0f, 0.25f, 0.25f, 0.5f, 0.5f, 0.75f, 0.75f, 1.0f, 1.0f};
// Development of one request, from the clip's recorded IPP2 settings (or its RMD's; as-shot ISO,
// white balance and exposure are kept unless `adjust` replaces them): the colour space and gamma
// curve, and for a display curve the tone map, highlight rolloff and HDR peak (0 keeps the
// recorded choice); the processing options. Anything out of range is a malformed request.
//
// The pixel type follows the gamma: the SDK's half-float output is always linear light (its header
// says the gamma curve is ignored, and with it the IPP2 output transform, the curves and the CDL),
// so linear decodes as half floats in the chosen primaries and every other curve as 16-bit
// integer RGB, 0..65535 code values of the curve, which the app linearises. The graded pipeline
// with nothing graded is used for a log output: the GPU decoder rejects Primary_Development_Only
// (InvalidJobParameter). Both pixel types are 6 bytes a pixel.
R3DSDK::VideoPixelType develop(R3DSDK::ImageProcessingSettings& s, const Develop& d) {
    using Limits = R3DSDK::ImageProcessingLimits;
    static const R3DSDK::ImageColorSpace spaces[] = {R3DSDK::ImageColorRec709, R3DSDK::ImageColorRec2020, R3DSDK::ImageColorDCIP3D65, R3DSDK::ImageColorREDWideGamutRGB};
    static const R3DSDK::ImageGammaCurve gammas[] = {R3DSDK::ImageGammaLinear, R3DSDK::ImageGammaBT1886, R3DSDK::ImageGammaSRGB,
                                                     R3DSDK::ImageGammaLog3G10, R3DSDK::ImageGammaHDR2084, R3DSDK::ImageGammaHybridLogGamma};
    static const R3DSDK::ToneMap tone_maps[] = {R3DSDK::ToneMap_None, R3DSDK::ToneMap_Low, R3DSDK::ToneMap_Medium, R3DSDK::ToneMap_High};
    static const R3DSDK::RollOff roll_offs[] = {R3DSDK::RollOff_None, R3DSDK::RollOff_Hard, R3DSDK::RollOff_Medium, R3DSDK::RollOff_Soft, R3DSDK::RollOff_Very_Soft};
    static const R3DSDK::FlashingPixelAdjust flashing[] = {R3DSDK::FlashingPixelAdjustOff, R3DSDK::FlashingPixelAdjustMild, R3DSDK::FlashingPixelAdjustMedium,
                                                           R3DSDK::FlashingPixelAdjustStrong};
    require(d.half <= 1 && d.space <= 3 && d.gamma <= 5 && d.tonemap <= 4 && d.rolloff <= 5 && d.options <= 31 && d.flashing <= 4,
            "unsupported development setting");
    require(supported(d.space, d.gamma), "unsupported colour space and gamma curve");
    require((d.options & 1) || !(d.options & 2), "malformed chroma noise reduction");
    if (d.peak != 0) {
        if (!(d.peak >= Limits::HdrPeakNitsMin && d.peak <= Limits::HdrPeakNitsMax)) throw std::runtime_error("HDR peak out of range: " + std::to_string(d.peak));
        s.HdrPeakNits = static_cast<unsigned int>(d.peak);
    }
    s.Version = R3DSDK::ColorVersion3;
    s.ImagePipelineMode = R3DSDK::Full_Graded;
    s.ColorSpace = spaces[d.space];
    s.GammaCurve = gammas[d.gamma];
    if (d.options & 1) s.ChromaNoiseReduction = (d.options & 2) != 0;
    if (d.flashing != 0) s.FlashingPixelAdjustment = flashing[d.flashing - 1];
    if (!(d.options & 4)) {
        for (int i = 0; i < 10; ++i) {
            s.UserCurve[i] = IDENTITY_CURVE[i];
            s.UserCurveRed[i] = IDENTITY_CURVE[i];
            s.UserCurveGreen[i] = IDENTITY_CURVE[i];
            s.UserCurveBlue[i] = IDENTITY_CURVE[i];
        }
    }
    if (d.gamma == 0) {
        s.OutputToneMap = R3DSDK::ToneMap_None;
        s.HighlightRollOff = R3DSDK::RollOff_None;
        s.Lut3DEnabled = false;
        s.CdlEnabled = false;
        return R3DSDK::PixelType_HalfFloat_RGB_Interleaved;
    }
    if (d.gamma == 3) {
        s.OutputToneMap = R3DSDK::ToneMap_None;
        s.HighlightRollOff = R3DSDK::RollOff_None;
    } else {
        if (d.tonemap != 0) s.OutputToneMap = tone_maps[d.tonemap - 1];
        if (d.rolloff != 0) s.HighlightRollOff = roll_offs[d.rolloff - 1];
    }
    s.Lut3DEnabled = (d.options & 8) != 0 && s.Lut3D != nullptr;
    s.CdlEnabled = (d.options & 16) != 0;
    return R3DSDK::PixelType_16Bit_RGB_Interleaved;
}

// The RAW adjustments of one request (FCRD7), applied before development: `flags` says which of
// ISO (bit 0), Kelvin (bit 1), tint (bit 2) and exposure (bit 3) replace the clip's recorded
// value; a clear bit keeps it and its field must be 0. Tint and exposure arrive in hundredths.
// Every value is checked against the SDK's own limits (ImageProcessingLimits, so a future SDK's
// wider range is honoured by this check and refused by the app's first): anything else is a
// malformed request, answered with an error, never applied.
void adjust(R3DSDK::ImageProcessingSettings& s, uint64_t flags, uint64_t iso, uint64_t kelvin, int64_t tint, int64_t exposure) {
    using Limits = R3DSDK::ImageProcessingLimits;
    require(flags <= 15, "unsupported RAW adjustment flags");
    if (flags & 1) {
        bool listed = false;
        for (size_t i = 0; i < Limits::ISOCount; ++i) listed = listed || Limits::ISOList[i] == iso;
        if (!listed) throw std::runtime_error("unsupported ISO " + std::to_string(iso));
        s.ISO = static_cast<size_t>(iso);
    } else {
        require(iso == 0, "malformed RAW adjustment");
    }
    if (flags & 2) {
        const double k = static_cast<double>(kelvin);
        if (!(k >= Limits::KelvinMin && k <= Limits::KelvinMax)) throw std::runtime_error("colour temperature out of range: " + std::to_string(kelvin) + " K");
        s.Kelvin = static_cast<float>(k);
    } else {
        require(kelvin == 0, "malformed RAW adjustment");
    }
    if (flags & 4) {
        const double t = static_cast<double>(tint) / 100.0;
        if (!(t >= Limits::TintMin && t <= Limits::TintMax)) throw std::runtime_error("tint out of range: " + std::to_string(t));
        s.Tint = static_cast<float>(t);
    } else {
        require(tint == 0, "malformed RAW adjustment");
    }
    if (flags & 8) {
        const double e = static_cast<double>(exposure) / 100.0;
        if (!(e >= Limits::ExposureAdjustMin && e <= Limits::ExposureAdjustMax)) throw std::runtime_error("exposure adjustment out of range: " + std::to_string(e) + " stops");
        s.ExposureAdjust = static_cast<float>(e);
    } else {
        require(exposure == 0, "malformed RAW adjustment");
    }
}

std::string clip_metadata(ClipState& c, size_t slots, size_t slot_bytes, const std::string& backend);
// CPU work of the helper: audio of any clip, and video when there is no GPU pipeline (the CPU
// helper serves thumbnails and the frames of a GPU that is unavailable). A few threads take it
// from two queues, audio first, so a thumbnail never makes the sound wait; the stdin thread only
// queues, so a slot release (`F`) is never stuck behind a decode. Different clips decode in
// parallel; one clip's decodes take turns (`ClipState::decode`).
struct CpuWork {
    struct Job {
        uint64_t id = 0;
        std::shared_ptr<ClipState> clip;
        bool audio = false;
        // a clip open (`O`): opened on a pool thread, so a card's worth of opens at import run
        // several at a time instead of one after another on the stdin thread (~5 ms each)
        bool open = false;
        // an RMD sidecar write (`W`): on a pool thread, holding the clip's decode lock
        bool rmd = false;
        Develop develop;
        uint64_t handle = 0;
        std::string path;
        size_t slots = 0, slot_bytes = 0;
        std::string backend;
        uint64_t start = 0, count = 0;
        // video
        R3DSDK::VideoDecodeMode mode = R3DSDK::DECODE_HALF_RES_PREMIUM;
        R3DSDK::VideoPixelType pixel_type = R3DSDK::PixelType_HalfFloat_RGB_Interleaved;
        R3DSDK::ImageProcessingSettings settings;
        size_t bytes = 0;
    };
    std::mutex mutex;
    std::condition_variable changed;
    std::deque<Job> audio, video;
    bool stopping = false;
    std::vector<std::thread> threads;
    const bool timing = std::getenv("FILMCRAFT_R3D_TIMING") != nullptr;

    explicit CpuWork(size_t count) {
        for (size_t i = 0; i < count; ++i) {
            threads.emplace_back([this] {
                try { serve(); }
                catch (const std::exception& e) {
                    try { error(0, e.what()); } catch (...) {}
                    std::_Exit(1);
                } catch (...) { std::_Exit(1); }
            });
        }
    }
    ~CpuWork() {
        {
            std::lock_guard<std::mutex> lock(mutex);
            stopping = true;
            changed.notify_all();
        }
        for (auto& t : threads) {
            if (t.joinable()) t.join();
        }
    }
    // Threads for CPU work: four by default (the SDK's own decoder threads do the heavy part).
    static size_t thread_count() { return env_number("FILMCRAFT_R3D_CPU_THREADS", 1, 16, 4); }
    void enqueue(Job job) {
        std::lock_guard<std::mutex> lock(mutex);
        // opens and audio are short and someone waits on them: they go before frames
        auto& queue = (job.audio || job.open || job.rmd) ? audio : video;
        require(queue.size() < MAX_WAITING, "too many requests outstanding");
        queue.push_back(std::move(job));
        changed.notify_one();
    }
    void serve() {
        for (;;) {
            Job job;
            {
                std::unique_lock<std::mutex> lock(mutex);
                changed.wait(lock, [&] { return stopping || !audio.empty() || !video.empty(); });
                if (!audio.empty()) {
                    job = std::move(audio.front());
                    audio.pop_front();
                } else if (!video.empty()) {
                    job = std::move(video.front());
                    video.pop_front();
                } else {
                    return; // stopping, nothing left
                }
            }
            try {
                run(job);
            } catch (const std::exception& e) {
                error(job.id, e.what());
            }
        }
    }
    void run(Job& job) {
        if (job.rmd) {
            auto& c = *job.clip;
            std::lock_guard<std::mutex> lock(c.decode);
            auto settings = c.settings();
            develop(settings, job.develop);
            adjust(settings, job.develop.flags, job.develop.iso, job.develop.kelvin, job.develop.tint, job.develop.exposure);
            require(c.clip->CreateOrUpdateRmd(settings), "RED SDK could not write the RMD sidecar");
            describe(c, true);
            const auto metadata = clip_metadata(c, job.slots, job.slot_bytes, job.backend);
            reply(job.id, 0, metadata.data(), metadata.size());
            return;
        }
        if (job.open) {
            auto state = open_clip(job.path, job.slot_bytes);
            {
                std::lock_guard<std::mutex> lock(clips_mutex);
                require(clips.size() < MAX_CLIPS || clips.count(job.handle), "too many open clips");
                clips[job.handle] = state;
            }
            const auto metadata = clip_metadata(*state, job.slots, job.slot_bytes, job.backend);
            reply(job.id, 0, metadata.data(), metadata.size());
            return;
        }
        auto& c = *job.clip;
        std::lock_guard<std::mutex> lock(c.decode);
        if (job.audio) {
            const size_t bytes = static_cast<size_t>(job.count) * c.channels * 4;
            Aligned buffer(bytes);
            size_t samples = static_cast<size_t>(job.count);
            auto result = c.floating ? c.clip->DecodeFloatAudio(job.start, &samples, buffer.data.get(), bytes)
                                     : c.clip->DecodeAudio(job.start, &samples, buffer.data.get(), bytes);
            require(result == R3DSDK::DSDecodeOK && samples <= job.count, "RED audio decode failed");
            reply(job.id, 0, buffer.data.get(), samples * c.channels * 4);
            return;
        }
        Aligned buffer(job.bytes);
        sentinel(buffer.data.get(), job.bytes);
        R3DSDK::VideoDecodeJob decode;
        decode.Mode = job.mode;
        decode.PixelType = job.pixel_type;
        decode.OutputBuffer = buffer.data.get();
        decode.OutputBufferSize = job.bytes;
        decode.ImageProcessing = &job.settings;
        decode.HdrProcessing = nullptr;
        decode.OutputFrameMetadata = nullptr;
        const auto begin = std::chrono::steady_clock::now();
        require(c.clip->DecodeVideoFrame(static_cast<size_t>(job.start), decode) == R3DSDK::DSDecodeOK, "RED video decode failed");
        if (timing)
            std::fprintf(stderr, "R3D frame %llu SDK %.3f ms (CPU)\n", static_cast<unsigned long long>(job.start),
                std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - begin).count());
        reply(job.id, 0, buffer.data.get(), job.bytes);
    }
};

// A recorded SDK setting as the request code that names it (0 when it is none of them).
template <typename T, size_t N> uint64_t code_of(T value, const T (&values)[N]) {
    for (size_t i = 0; i < N; ++i) {
        if (values[i] == value) return i + 1;
    }
    return 0;
}
bool curves_recorded(const R3DSDK::ImageProcessingSettings& s) {
    for (int i = 0; i < 10; ++i) {
        if (s.UserCurve[i] != IDENTITY_CURVE[i] || s.UserCurveRed[i] != IDENTITY_CURVE[i] || s.UserCurveGreen[i] != IDENTITY_CURVE[i] ||
            s.UserCurveBlue[i] != IDENTITY_CURVE[i])
            return true;
    }
    return false;
}
// The clip's metadata for the app (FCRD9, the reply to `O` and `W`): dimensions, timing, audio,
// this helper's slots and slot size; then its timecode, the processing backend, the as-shot ISO,
// white balance and exposure adjustment; then what else it records (the request codes of its
// colour space, gamma, tone map, rolloff and flashing pixel adjustment plus one, 0 for one
// without a code; its HDR peak, chroma noise reduction, whether it carries a creative LUT and
// asks for it and for its CDL, whether its curves are not the identity, its video track count,
// whether an RMD sidecar is present, the SDK's HDR peak range and default, its default tone map
// and rolloff codes, and the CDL: saturation, then slope, offset and power of red, green and
// blue); then the RMD sidecar's path; then the recorded lens squeeze (0: none recorded).
std::string clip_metadata(ClipState& c, size_t slots, size_t slot_bytes, const std::string& backend) {
    using Limits = R3DSDK::ImageProcessingLimits;
    static const R3DSDK::ImageColorSpace spaces[] = {R3DSDK::ImageColorRec709, R3DSDK::ImageColorRec2020, R3DSDK::ImageColorDCIP3D65, R3DSDK::ImageColorREDWideGamutRGB};
    static const R3DSDK::ImageGammaCurve gammas[] = {R3DSDK::ImageGammaLinear, R3DSDK::ImageGammaBT1886, R3DSDK::ImageGammaSRGB,
                                                     R3DSDK::ImageGammaLog3G10, R3DSDK::ImageGammaHDR2084, R3DSDK::ImageGammaHybridLogGamma};
    static const R3DSDK::ToneMap tone_maps[] = {R3DSDK::ToneMap_None, R3DSDK::ToneMap_Low, R3DSDK::ToneMap_Medium, R3DSDK::ToneMap_High};
    static const R3DSDK::RollOff roll_offs[] = {R3DSDK::RollOff_None, R3DSDK::RollOff_Hard, R3DSDK::RollOff_Medium, R3DSDK::RollOff_Soft, R3DSDK::RollOff_Very_Soft};
    static const R3DSDK::FlashingPixelAdjust flashing[] = {R3DSDK::FlashingPixelAdjustOff, R3DSDK::FlashingPixelAdjustMild, R3DSDK::FlashingPixelAdjustMedium,
                                                           R3DSDK::FlashingPixelAdjustStrong};
    std::ostringstream info;
    info << "FCRD9 " << c.width << ' ' << c.height << ' ' << c.frames << ' ' << c.num << ' ' << c.den << ' '
         << c.channels << ' ' << c.rate << ' ' << c.samples << ' ' << c.floating << ' ' << c.bits << ' '
         << slots << ' ' << slot_bytes << '\n';
    const char* tc = c.clip->Timecode(0);
    if (tc) info << tc;
    info << '\n' << backend;
    std::lock_guard<std::mutex> lock(c.settings_mutex);
    const auto& s = c.base;
    info << '\n' << s.ISO << ' ' << std::setprecision(9) << s.Kelvin << ' ' << s.Tint << ' ' << s.ExposureAdjust;
    info << '\n' << code_of(s.ColorSpace, spaces) << ' ' << code_of(s.GammaCurve, gammas) << ' ' << code_of(s.OutputToneMap, tone_maps) << ' '
         << code_of(s.HighlightRollOff, roll_offs) << ' ' << s.HdrPeakNits << ' ' << (s.ChromaNoiseReduction ? 1 : 0) << ' '
         << code_of(s.FlashingPixelAdjustment, flashing) << ' ' << (c.lut ? 1 : 0) << ' ' << (s.Lut3DEnabled ? 1 : 0) << ' ' << (s.CdlEnabled ? 1 : 0) << ' '
         << (curves_recorded(s) ? 1 : 0) << ' ' << c.tracks << ' ' << (c.rmd ? 1 : 0) << ' ' << Limits::HdrPeakNitsMin << ' ' << Limits::HdrPeakNitsMax << ' '
         << Limits::HdrPeakNitsDefault << ' ' << code_of(Limits::OutputToneMapDefault, tone_maps) << ' ' << code_of(Limits::HighlightRollOffDefault, roll_offs)
         << ' ' << s.CdlSaturation;
    for (const auto* sop : {&s.CdlRed, &s.CdlGreen, &s.CdlBlue}) info << ' ' << sop->Slope << ' ' << sop->Offset << ' ' << sop->Power;
    info << '\n' << c.rmd_path;
    info << '\n' << c.squeeze;
    return info.str();
}

// Longest command line: an `O` carries a path.
constexpr size_t MAX_LINE = 16384;

void run(const char* libraries, const Name& name, size_t requested_slot_bytes) {
    const char* requested = std::getenv("FILMCRAFT_R3D_PROCESSING");
    require(!requested || std::strcmp(requested, "gpu") == 0 || std::strcmp(requested, "cpu") == 0,
            "FILMCRAFT_R3D_PROCESSING must be gpu or cpu");
    const bool use_gpu = !requested || std::strcmp(requested, "cpu") != 0;
    Sdk sdk(libraries, use_gpu);
    // Declared after the SDK, so destroyed before it: the open clips go before FinalizeSdk (a clip
    // released after it faults), and after the pipeline and the CPU work, which may hold them.
    struct CloseClips {
        ~CloseClips() {
            std::lock_guard<std::mutex> lock(clips_mutex);
            clips.clear();
        }
    } close_clips;
    std::unique_ptr<Gpu> gpu;
    if (use_gpu) gpu = std::make_unique<Gpu>();
#ifndef _WIN32
    if (gpu) sdk.finalize = false;
#endif
    // Built before the start-up reply so a failed job allocation or mapping is the start-up
    // error, and the slot count and size reach the app (it keeps that many requests outstanding,
    // plus one, and maps the slots). The slots hold the largest frame the app expects; a clip
    // larger than that is refused at `O` and the app starts a helper with larger slots.
    std::unique_ptr<Shared> shared;
    std::unique_ptr<Pipeline> pipeline;
    if (gpu) {
        require(requested_slot_bytes > 0 && requested_slot_bytes <= MAX_PIXELS * 6, "invalid frame slot size");
        shared = std::make_unique<Shared>(name, Pipeline::slot_count(), requested_slot_bytes);
        pipeline = std::make_unique<Pipeline>(*gpu, *shared);
    }
    const size_t slots = pipeline ? pipeline->count : 0, slot_bytes = shared ? shared->slot_bytes : 0;
    const std::string backend = gpu ? gpu->description : "CPU (explicit override)";
    CpuWork cpu(CpuWork::thread_count());
    {
        std::ostringstream info;
        info << "FCRD9 " << slots << ' ' << slot_bytes << ' ' << cpu.threads.size() << '\n' << backend;
        const auto start = info.str();
        reply(0, 0, start.data(), start.size());
    }

    std::string line;
    while (std::getline(std::cin, line)) {
        uint64_t id = 0;
        try {
            require(line.size() <= MAX_LINE, "decode request exceeds line limit");
            std::istringstream command(line);
            char op = 0;
            uint64_t start = 0, count = 0, handle = 0;
            std::string extra;
            command >> op;
            if (op == 'F') {
                command >> start;
                if (command.fail() || (command >> extra)) throw fatal("malformed frame release");
                if (!pipeline) throw fatal("frame release without shared frames");
                pipeline->release(start);
                continue;
            }
            if (op == 'C') {
                command >> handle;
                if (command.fail() || (command >> extra)) throw fatal("malformed clip close");
                std::shared_ptr<ClipState> closing;
                {
                    std::lock_guard<std::mutex> lock(clips_mutex);
                    const auto it = clips.find(handle);
                    if (it != clips.end()) {
                        closing = std::move(it->second);
                        clips.erase(it);
                    }
                }
                continue; // freed here, or by the last request that still holds it
            }
            command >> id >> handle;
            require(!command.fail() && id != 0 && handle != 0, "malformed decode request");
            if (op == 'O') {
                // the rest of the line after one space is the path (UTF-8, spaces allowed)
                std::string path;
                std::getline(command, path);
                require(path.size() > 1 && path[0] == ' ', "malformed clip open");
                path.erase(0, 1);
                CpuWork::Job job;
                job.id = id;
                job.open = true;
                job.handle = handle;
                job.path = std::move(path);
                job.slots = slots;
                job.slot_bytes = slot_bytes;
                job.backend = backend;
                cpu.enqueue(std::move(job));
                continue;
            }
            auto clip = find_clip(handle);
            if (op == 'W') {
                // `W <id> <clip> <development fields>`: write them into the clip's RMD sidecar
                CpuWork::Job job;
                require(read_develop(command, job.develop, false) && !(command >> extra), "malformed RMD write");
                // checked here as well, so a malformed write is answered before it is queued
                R3DSDK::ImageProcessingSettings check = clip->settings();
                develop(check, job.develop);
                adjust(check, job.develop.flags, job.develop.iso, job.develop.kelvin, job.develop.tint, job.develop.exposure);
                job.id = id;
                job.clip = clip;
                job.rmd = true;
                job.slots = slots;
                job.slot_bytes = slot_bytes;
                job.backend = backend;
                cpu.enqueue(std::move(job));
                continue;
            }
            command >> start;
            require(!command.fail(), "malformed decode request");
            if (op == 'V') {
                Develop d;
                command >> count;
                require(!command.fail() && read_develop(command, d, true) && !(command >> extra) && start < clip->frames, "video frame out of range");
                require(count == 1 || count == 2 || count == 4 || count == 8 || count == 16, "unsupported preview divisor");
                // SDK 9.2.1 writes floor(w/count) x floor(h/count) pixels, packed at that width whatever
                // bytesPerRow says, and nothing past OutputBufferSize (measured; docs/r3d-ne.md). The app
                // computes the same size and rejects any other reply length.
                const size_t rw = clip->width / count, rh = clip->height / count;
                require(rw > 0 && rh > 0, "unsupported preview divisor");
                R3DSDK::ImageProcessingSettings settings = clip->settings();
                const auto pixel_type = develop(settings, d);
                adjust(settings, d.flags, d.iso, d.kelvin, d.tint, d.exposure);
                // half-float and 16-bit integer RGB are both 6 bytes per pixel
                const size_t bytes = rw * rh * 6;
                const auto mode = count == 1 ? R3DSDK::DECODE_FULL_RES_PREMIUM : count == 2 ? (d.half ? R3DSDK::DECODE_HALF_RES_GOOD : R3DSDK::DECODE_HALF_RES_PREMIUM)
                                : count == 4 ? R3DSDK::DECODE_QUARTER_RES_GOOD : count == 8 ? R3DSDK::DECODE_EIGHT_RES_GOOD
                                : R3DSDK::DECODE_SIXTEENTH_RES_GOOD;
                if (pipeline) {
                    Pipeline::Request request;
                    request.id = id;
                    request.clip = clip;
                    request.frame = static_cast<size_t>(start);
                    request.divisor = count;
                    request.bytes = bytes;
                    request.width = rw;
                    request.mode = mode;
                    request.settings = settings;
                    request.pixel_type = pixel_type;
                    pipeline->enqueue(request);
                    continue;
                }
                CpuWork::Job job;
                job.id = id;
                job.clip = clip;
                job.start = start;
                job.mode = mode;
                job.pixel_type = pixel_type;
                job.settings = settings;
                job.bytes = bytes;
                cpu.enqueue(std::move(job));
            } else if (op == 'A') {
                command >> count;
                require(!command.fail() && !(command >> extra), "malformed audio request");
                require(clip->channels > 0 && count > 0 && count <= MAX_AUDIO && start < clip->samples, "audio request out of range");
                CpuWork::Job job;
                job.id = id;
                job.clip = clip;
                job.audio = true;
                job.start = start;
                job.count = count;
                cpu.enqueue(std::move(job));
            } else {
                throw std::runtime_error("unknown decode request");
            }
        } catch (const fatal& e) {
            // Reported before anything is torn down (the SDK's teardown can take a while, and
            // callbacks may still reference the slots), then out, as the pipeline thread does.
            std::fprintf(stderr, "filmcraft-red-helper: %s\n", e.what());
            try { error(0, e.what()); } catch (...) {}
            std::_Exit(1);
        } catch (const std::exception& e) {
            if (id != 0) {
                error(id, e.what());
            } else {
                // a line too malformed to name its request cannot be answered: the state is no
                // longer shared, so the helper ends
                std::fprintf(stderr, "filmcraft-red-helper: %s\n", e.what());
                try { error(0, e.what()); } catch (...) {}
                std::_Exit(1);
            }
        }
    }
    require(std::cin.eof(), "decode request exceeds line limit");
}
}

constexpr const char* USAGE = "usage: filmcraft-red-helper <SDK runtime directory> <reply pipe name> <frame slot bytes>";
// The frame slot size argument: decimal bytes (0 for a CPU-only helper).
size_t slot_bytes_argument(const char* text) {
    char* end = nullptr;
    const auto n = std::strtoull(text, &end, 10);
    require(end != text && *end == 0 && n <= MAX_PIXELS * 6, "invalid frame slot size");
    return static_cast<size_t>(n);
}
int serve(const char* libraries, const Name::value_type* name_argument, const char* slot_bytes) {
    try {
        const auto name = checked_name(name_argument);
        open_output(name);
        run(libraries, name, slot_bytes_argument(slot_bytes));
        return 0;
    } catch (const std::exception& e) {
        try { error(0, e.what()); } catch (...) {}
    } catch (...) {
        try { error(0, "unexpected RED SDK failure"); } catch (...) {}
    }
    return 1;
}

#ifdef _WIN32
int wmain(int argc, wchar_t** argv) {
    _setmode(_fileno(stdout), _O_BINARY);
    // Do not display Windows fault dialogs for a failed worker.
    SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
    try {
        require(argc == 4, USAGE);
        auto libraries = utf8(argv[1]), slots = utf8(argv[3]);
        return serve(libraries.c_str(), argv[2], slots.c_str());
    } catch (const std::exception& e) {
        try { error(0, e.what()); } catch (...) {}
    }
    return 1;
}
#else
int main(int argc, char** argv) {
    // A closed reply pipe must surface as a write error (the writer thread then exits the
    // helper), not as a silent SIGPIPE death. Paths arrive as UTF-8 bytes already.
    std::signal(SIGPIPE, SIG_IGN);
    if (argc != 4) {
        try { error(0, USAGE); } catch (...) {}
        return 1;
    }
    return serve(argv[1], argv[2], argv[3]);
}
#endif
