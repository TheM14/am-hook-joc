// Reference dump tool for the am-hook-joc comparison harness.
//
// It drives the *shipped* joc_core.dll through its public streaming API, so the
// samples it writes come from exactly the code joc_cli.exe uses.  Three output
// kinds are available: the reconstructed objects16 frame, the 2.0 speaker mix,
// and the binaural render.  Input is a bare E-AC-3 stream plus the predecoded
// core PCM, the same pair the Rust implementation reads.

#include <cstdint>
#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

#include "joc_stream.h"

namespace {

struct Options {
    std::string input;
    std::string bed;
    std::string out;
    std::string hrtf;
    std::string layout = "2.0";
    unsigned bed_channels = 6;
    unsigned mode = 0;  // 0 objects16, 1 speaker, 2 binaural
    double gain_db = 0.0;
    unsigned long frames = 0;
    unsigned long skip = 0;
};

bool read_file(const std::string& path, std::vector<std::uint8_t>* out) {
    std::FILE* file = std::fopen(path.c_str(), "rb");
    if (file == nullptr) {
        return false;
    }
    std::fseek(file, 0, SEEK_END);
    const long size = std::ftell(file);
    std::fseek(file, 0, SEEK_SET);
    out->resize(static_cast<std::size_t>(size));
    const std::size_t read = std::fread(out->data(), 1, out->size(), file);
    std::fclose(file);
    return read == out->size();
}

bool write_file(const std::string& path, const std::vector<float>& values) {
    std::FILE* file = std::fopen(path.c_str(), "wb");
    if (file == nullptr) {
        return false;
    }
    std::fwrite(values.data(), sizeof(float), values.size(), file);
    std::fclose(file);
    return true;
}

const char* last_error(const joc_stream* stream) {
    static const char* no_detail = "";
    joc_stream_status_info info{};
    (void)info;
    (void)stream;
    return no_detail;
}

}  // namespace

int main(int argc, char** argv) {
    Options options;
    for (int index = 1; index < argc; ++index) {
        const std::string arg = argv[index];
        const auto value = [&]() -> std::string {
            if (index + 1 >= argc) {
                std::fprintf(stderr, "%s needs a value\n", arg.c_str());
                std::exit(2);
            }
            return argv[++index];
        };
        if (arg == "--input") options.input = value();
        else if (arg == "--bed") options.bed = value();
        else if (arg == "--out") options.out = value();
        else if (arg == "--hrtf") options.hrtf = value();
        else if (arg == "--layout") options.layout = value();
        else if (arg == "--bed-channels") options.bed_channels = std::stoul(value());
        else if (arg == "--gain-db") options.gain_db = std::stod(value());
        else if (arg == "--frames") options.frames = std::stoul(value());
        else if (arg == "--skip-frames") options.skip = std::stoul(value());
        else if (arg == "--mode") {
            const std::string mode = value();
            options.mode = mode == "objects16" ? 0 : (mode == "speaker" ? 1 : 2);
        } else {
            std::fprintf(stderr, "unknown option %s\n", arg.c_str());
            return 2;
        }
    }

    std::vector<std::uint8_t> stream_bytes;
    std::vector<std::uint8_t> bed_bytes;
    if (!read_file(options.input, &stream_bytes) || !read_file(options.bed, &bed_bytes)) {
        std::fprintf(stderr, "cannot read input or bed\n");
        return 1;
    }
    const float* bed = reinterpret_cast<const float*>(bed_bytes.data());
    const std::size_t bed_samples = bed_bytes.size() / sizeof(float);

    joc_stream_config config{};
    config.struct_size = sizeof(config);
    config.struct_version = 1;
    config.input = JOC_STREAM_IN_EAC3;
    config.output = options.mode == 0 ? JOC_STREAM_OUT_PCM_OBJECTS16
                    : (options.mode == 1 ? JOC_STREAM_OUT_SPEAKER : JOC_STREAM_OUT_BINAURAL);
    config.speaker_layout_name = options.layout.c_str();
    config.binaural_mode = JOC_BINAURAL_MID;
    config.hrtf_path = options.hrtf.empty() ? nullptr : options.hrtf.c_str();
    config.binaural_tail_seconds = 5.0;
    config.object_delay_samples = 1473;
    config.gain_db = options.gain_db;
    config.native_threads = 1;

    joc_stream* stream = nullptr;
    const joc_error created = joc_stream_create(&config, &stream);
    if (created != JOC_OK || stream == nullptr) {
        std::fprintf(stderr, "joc_stream_create failed: %s\n", joc_error_name(created));
        return 1;
    }

    const unsigned channels = options.mode == 0 ? 16 : (options.mode == 1 ? 2 : 2);
    std::vector<float> output;
    std::vector<float> scratch(static_cast<std::size_t>(16) * 1536 * 2);

    std::size_t offset = 0;
    unsigned long frame = 0;
    while (offset + 4 <= stream_bytes.size()) {
        const std::uint16_t sync = static_cast<std::uint16_t>(
            (stream_bytes[offset] << 8) | stream_bytes[offset + 1]);
        if (sync != 0x0B77) {
            std::fprintf(stderr, "lost sync at %zu\n", offset);
            return 1;
        }
        const unsigned bsid = (stream_bytes[offset + 5] >> 3) & 0x1F;
        std::size_t frame_bytes = 0;
        if (bsid <= 8) {
            std::fprintf(stderr, "AC-3 frame at %zu is not supported\n", offset);
            return 1;
        }
        const unsigned frmsiz = ((stream_bytes[offset + 2] & 0x07) << 8) | stream_bytes[offset + 3];
        frame_bytes = (static_cast<std::size_t>(frmsiz) + 1) * 2;
        if (offset + frame_bytes > stream_bytes.size()) {
            break;
        }
        if (options.skip != 0 && frame < options.skip) {
            offset += frame_bytes;
            ++frame;
            continue;
        }
        if (options.frames != 0 && frame >= options.frames + options.skip) {
            break;
        }

        joc_stream_buffer input{};
        input.struct_size = sizeof(input);
        input.struct_version = 1;
        input.kind = JOC_STREAM_IN_EAC3;
        input.bytes = stream_bytes.data() + offset;
        input.byte_count = static_cast<std::uint32_t>(frame_bytes);
        std::uint32_t consumed_samples = 0;
        std::uint32_t consumed_bytes = 0;
        joc_error error = joc_stream_push(stream, &input, &consumed_samples, &consumed_bytes);
        if (error != JOC_OK) {
            std::fprintf(stderr, "push eac3 failed at frame %lu: %s\n", frame, joc_error_name(error));
            return 1;
        }
        const std::size_t bed_offset = static_cast<std::size_t>(frame) * 1536u * options.bed_channels;
        if (bed_offset + 1536u * options.bed_channels <= bed_samples) {
            joc_stream_buffer pcm{};
            pcm.struct_size = sizeof(pcm);
            pcm.struct_version = 1;
            pcm.kind = JOC_STREAM_IN_CORE_PCM;
            pcm.channels = options.bed_channels;
            pcm.sample_rate = 48000;
            pcm.sample_count = 1536;
            pcm.byte_count = 1536u * options.bed_channels * sizeof(float);
            pcm.pcm = bed + bed_offset;
            error = joc_stream_push(stream, &pcm, &consumed_samples, &consumed_bytes);
            if (error != JOC_OK) {
                std::fprintf(stderr, "push bed failed at frame %lu: %s\n", frame, joc_error_name(error));
                return 1;
            }
        }
        for (;;) {
            joc_stream_buffer out{};
            out.struct_size = sizeof(out);
            out.struct_version = 1;
            out.kind = config.output;
            out.channels = channels;
            out.out_pcm = scratch.data();
            out.sample_count = static_cast<std::uint32_t>(scratch.size() / channels);
            std::uint32_t produced = 0;
            error = joc_stream_pull(stream, &out, &produced);
            if (error != JOC_OK) {
                std::fprintf(stderr, "pull failed: %s\n", joc_error_name(error));
                return 1;
            }
            if (produced == 0) {
                break;
            }
            output.insert(output.end(), scratch.begin(), scratch.begin() + produced * channels);
        }
        offset += frame_bytes;
        ++frame;
    }

    joc_error error = joc_stream_flush(stream);
    if (error != JOC_OK) {
        std::fprintf(stderr, "flush failed: %s\n", joc_error_name(error));
        return 1;
    }
    for (;;) {
        joc_stream_buffer out{};
        out.struct_size = sizeof(out);
        out.struct_version = 1;
        out.kind = config.output;
        out.channels = channels;
        out.out_pcm = scratch.data();
        out.sample_count = static_cast<std::uint32_t>(scratch.size() / channels);
        std::uint32_t produced = 0;
        error = joc_stream_pull(stream, &out, &produced);
        if (error != JOC_OK || produced == 0) {
            break;
        }
        output.insert(output.end(), scratch.begin(), scratch.begin() + produced * channels);
    }

    joc_stream_status_info status{};
    status.struct_size = sizeof(status);
    status.struct_version = 1;
    joc_stream_status(stream, &status);
    std::fprintf(stderr,
                 "objects16: frames_in=%llu frames_out=%llu samples_out=%llu missing=%llu clamped=%llu\n",
                 static_cast<unsigned long long>(status.frames_in),
                 static_cast<unsigned long long>(status.frames_out),
                 static_cast<unsigned long long>(status.samples_out),
                 static_cast<unsigned long long>(status.missing_joc_frames),
                 static_cast<unsigned long long>(status.objects_clamped_frames));

    if (!write_file(options.out, output)) {
        std::fprintf(stderr, "cannot write %s\n", options.out.c_str());
        return 1;
    }
    std::fprintf(stderr, "wrote %zu floats (%u channels)\n", output.size(), channels);
    (void)last_error(stream);
    joc_stream_destroy(stream);
    return 0;
}
