#include <jni.h>

#include <cstdint>
#include <string>
#include <string_view>
#include <vector>

#include "../rish.h"

namespace {

constexpr char kNullRequest[] =
    R"({"protocol_version":1,"ok":false,"error":"null request"})";
constexpr char kInvalidUnicode[] =
    R"({"protocol_version":1,"ok":false,"error":"request contains invalid Unicode"})";
constexpr char kNullResponse[] =
    R"({"protocol_version":1,"ok":false,"error":"Rust JSON operation returned null"})";
constexpr char kInvalidResponse[] =
    R"({"protocol_version":1,"ok":false,"error":"Rust JSON operation returned invalid UTF-8"})";
constexpr char kRequestTooLarge[] =
    R"({"protocol_version":1,"ok":false,"error":"request exceeds platform bridge limit"})";
constexpr jsize kMaximumRequestCharacters = 8 * 1024 * 1024;

void AppendUtf8(std::uint32_t code_point, std::string *output) {
    if (code_point <= 0x7f) {
        output->push_back(static_cast<char>(code_point));
    } else if (code_point <= 0x7ff) {
        output->push_back(static_cast<char>(0xc0 | (code_point >> 6)));
        output->push_back(static_cast<char>(0x80 | (code_point & 0x3f)));
    } else if (code_point <= 0xffff) {
        output->push_back(static_cast<char>(0xe0 | (code_point >> 12)));
        output->push_back(
            static_cast<char>(0x80 | ((code_point >> 6) & 0x3f))
        );
        output->push_back(static_cast<char>(0x80 | (code_point & 0x3f)));
    } else {
        output->push_back(static_cast<char>(0xf0 | (code_point >> 18)));
        output->push_back(
            static_cast<char>(0x80 | ((code_point >> 12) & 0x3f))
        );
        output->push_back(
            static_cast<char>(0x80 | ((code_point >> 6) & 0x3f))
        );
        output->push_back(static_cast<char>(0x80 | (code_point & 0x3f)));
    }
}

bool JStringToUtf8(JNIEnv *env, jstring input, std::string *output) {
    const jsize length = env->GetStringLength(input);
    if (length > kMaximumRequestCharacters) {
        return false;
    }
    const jchar *characters = env->GetStringChars(input, nullptr);
    if (characters == nullptr) {
        return false;
    }

    output->clear();
    output->reserve(static_cast<std::size_t>(length));
    bool valid = true;
    for (jsize index = 0; index < length; ++index) {
        std::uint32_t code_point = characters[index];
        if (code_point >= 0xd800 && code_point <= 0xdbff) {
            if (index + 1 >= length) {
                valid = false;
                break;
            }
            const std::uint32_t low = characters[++index];
            if (low < 0xdc00 || low > 0xdfff) {
                valid = false;
                break;
            }
            code_point =
                0x10000 + ((code_point - 0xd800) << 10) + (low - 0xdc00);
        } else if (code_point >= 0xdc00 && code_point <= 0xdfff) {
            valid = false;
            break;
        }
        // The Rust C ABI is NUL-terminated. A raw U+0000 is invalid inside a
        // JSON document anyway; its escaped spelling reaches us as ASCII.
        if (code_point == 0) {
            valid = false;
            break;
        }
        AppendUtf8(code_point, output);
    }

    env->ReleaseStringChars(input, characters);
    return valid;
}

bool ReadContinuation(
    std::string_view input,
    std::size_t *index,
    std::uint32_t *value
) {
    if (*index >= input.size()) {
        return false;
    }
    const auto byte = static_cast<std::uint8_t>(input[(*index)++]);
    if ((byte & 0xc0) != 0x80) {
        return false;
    }
    *value = (*value << 6) | (byte & 0x3f);
    return true;
}

bool Utf8ToUtf16(std::string_view input, std::vector<jchar> *output) {
    output->clear();
    output->reserve(input.size());

    std::size_t index = 0;
    while (index < input.size()) {
        const auto lead = static_cast<std::uint8_t>(input[index++]);
        std::uint32_t code_point = 0;
        std::uint32_t minimum = 0;
        int continuation_count = 0;
        if (lead <= 0x7f) {
            code_point = lead;
        } else if ((lead & 0xe0) == 0xc0) {
            code_point = lead & 0x1f;
            minimum = 0x80;
            continuation_count = 1;
        } else if ((lead & 0xf0) == 0xe0) {
            code_point = lead & 0x0f;
            minimum = 0x800;
            continuation_count = 2;
        } else if ((lead & 0xf8) == 0xf0) {
            code_point = lead & 0x07;
            minimum = 0x10000;
            continuation_count = 3;
        } else {
            return false;
        }

        for (int count = 0; count < continuation_count; ++count) {
            if (!ReadContinuation(input, &index, &code_point)) {
                return false;
            }
        }
        if (code_point < minimum || code_point > 0x10ffff ||
            (code_point >= 0xd800 && code_point <= 0xdfff)) {
            return false;
        }

        if (code_point <= 0xffff) {
            output->push_back(static_cast<jchar>(code_point));
        } else {
            code_point -= 0x10000;
            output->push_back(
                static_cast<jchar>(0xd800 | (code_point >> 10))
            );
            output->push_back(
                static_cast<jchar>(0xdc00 | (code_point & 0x3ff))
            );
        }
    }
    return true;
}

jstring AsciiError(JNIEnv *env, const char *message) {
    // These static errors contain ASCII only, for which JNI modified UTF-8 and
    // standard UTF-8 are identical.
    return env->NewStringUTF(message);
}

jstring Utf8ToJString(JNIEnv *env, std::string_view input) {
    std::vector<jchar> utf16;
    if (!Utf8ToUtf16(input, &utf16)) {
        return nullptr;
    }
    if (utf16.empty()) {
        return env->NewStringUTF("");
    }
    return env->NewString(
        utf16.data(),
        static_cast<jsize>(utf16.size())
    );
}

using RustJsonOperation = char *(*)(const char *, size_t);

jstring InvokeJson(
    JNIEnv *env,
    jstring request,
    RustJsonOperation operation
) {
    if (request == nullptr) {
        return AsciiError(env, kNullRequest);
    }

    std::string request_utf8;
    if (!JStringToUtf8(env, request, &request_utf8)) {
        if (env->ExceptionCheck()) {
            return nullptr;
        }
        if (env->GetStringLength(request) > kMaximumRequestCharacters) {
            return AsciiError(env, kRequestTooLarge);
        }
        return AsciiError(env, kInvalidUnicode);
    }

    char *response = operation(request_utf8.data(), request_utf8.size());
    if (response == nullptr) {
        return AsciiError(env, kNullResponse);
    }

    jstring result = Utf8ToJString(env, response);
    rish_string_free(response);
    if (result == nullptr && !env->ExceptionCheck()) {
        return AsciiError(env, kInvalidResponse);
    }
    return result;
}

}  // namespace

extern "C" JNIEXPORT jstring JNICALL
Java_dev_rish_runtime_RishBridge_planJson(
    JNIEnv *env,
    jclass,
    jstring request
) {
    return InvokeJson(env, request, rish_plan_json);
}

extern "C" JNIEXPORT jstring JNICALL
Java_dev_rish_runtime_RishBridge_executeAppletJson(
    JNIEnv *env,
    jclass,
    jstring request
) {
    return InvokeJson(env, request, rish_execute_applet_json);
}

extern "C" JNIEXPORT jint JNICALL
Java_dev_rish_runtime_RishBridge_protocolVersion(
    JNIEnv *,
    jclass
) {
    return static_cast<jint>(rish_protocol_version());
}
