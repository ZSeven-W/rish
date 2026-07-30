#include <jni.h>

#include "../rish.h"

extern "C" JNIEXPORT jstring JNICALL
Java_dev_rish_runtime_RishBridge_planJson(
    JNIEnv *env,
    jclass,
    jstring request
) {
    if (request == nullptr) {
        return env->NewStringUTF(
            R"({"protocol_version":1,"ok":false,"error":"null request"})"
        );
    }

    const char *request_chars = env->GetStringUTFChars(request, nullptr);
    if (request_chars == nullptr) {
        return nullptr;
    }

    char *response = rish_plan_json(request_chars);
    env->ReleaseStringUTFChars(request, request_chars);
    if (response == nullptr) {
        return nullptr;
    }

    jstring result = env->NewStringUTF(response);
    rish_string_free(response);
    return result;
}
