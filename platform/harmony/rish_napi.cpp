#include <cstring>
#include <string>

#include "napi/native_api.h"
#include "napi/native_node_api.h"

#include "../rish.h"

namespace {

constexpr size_t kMaximumRequestBytes = 8 * 1024 * 1024;

using RustJsonOperation = char *(*)(const char *, size_t);

napi_value InvokeJson(
    napi_env env,
    napi_callback_info info,
    const char *operation_name,
    RustJsonOperation operation
) {
    size_t argc = 1;
    napi_value argv[1] = {nullptr};
    if (napi_get_cb_info(env, info, &argc, argv, nullptr, nullptr) != napi_ok) {
        napi_throw_error(env, nullptr, "failed to read JSON operation arguments");
        return nullptr;
    }

    if (argc != 1) {
        napi_throw_type_error(env, nullptr, operation_name);
        return nullptr;
    }

    size_t request_size = 0;
    if (napi_get_value_string_utf8(env, argv[0], nullptr, 0, &request_size) != napi_ok) {
        napi_throw_type_error(env, nullptr, "request must be a UTF-8 string");
        return nullptr;
    }
    if (request_size > kMaximumRequestBytes) {
        napi_throw_range_error(env, nullptr, "request exceeds platform bridge limit");
        return nullptr;
    }

    std::string request(request_size, '\0');
    if (napi_get_value_string_utf8(
            env,
            argv[0],
            request.data(),
            request.size() + 1,
            &request_size
        ) != napi_ok) {
        napi_throw_error(env, nullptr, "failed to decode request as UTF-8");
        return nullptr;
    }
    if (request.find('\0') != std::string::npos) {
        napi_throw_type_error(env, nullptr, "request contains an embedded NUL");
        return nullptr;
    }

    char *response = operation(request.data(), request.size());
    if (response == nullptr) {
        napi_throw_error(env, nullptr, "Rust JSON operation returned null");
        return nullptr;
    }

    napi_value result = nullptr;
    napi_create_string_utf8(env, response, std::strlen(response), &result);
    rish_string_free(response);
    return result;
}

napi_value PlanJson(napi_env env, napi_callback_info info) {
    return InvokeJson(
        env,
        info,
        "planJson expects one JSON string",
        rish_plan_json
    );
}

napi_value ExecuteAppletJson(napi_env env, napi_callback_info info) {
    return InvokeJson(
        env,
        info,
        "executeAppletJson expects one JSON string",
        rish_execute_applet_json
    );
}

napi_value VmRunDockerJson(napi_env env, napi_callback_info info) {
    return InvokeJson(
        env,
        info,
        "vmRunDockerJson expects one JSON string",
        rish_vm_run_docker_json
    );
}

napi_value ProtocolVersion(napi_env env, napi_callback_info) {
    napi_value result = nullptr;
    napi_create_uint32(env, rish_protocol_version(), &result);
    return result;
}

napi_value Init(napi_env env, napi_value exports) {
    napi_property_descriptor properties[] = {
        {"planJson", nullptr, PlanJson, nullptr, nullptr, nullptr, napi_default, nullptr},
        {
            "executeAppletJson",
            nullptr,
            ExecuteAppletJson,
            nullptr,
            nullptr,
            nullptr,
            napi_default,
            nullptr,
        },
        {
            "vmRunDockerJson",
            nullptr,
            VmRunDockerJson,
            nullptr,
            nullptr,
            nullptr,
            napi_default,
            nullptr,
        },
        {
            "protocolVersion",
            nullptr,
            ProtocolVersion,
            nullptr,
            nullptr,
            nullptr,
            napi_default,
            nullptr,
        },
    };
    napi_define_properties(
        env,
        exports,
        sizeof(properties) / sizeof(properties[0]),
        properties
    );
    return exports;
}

}  // namespace

static napi_module module = {
    1,
    0,
    nullptr,
    Init,
    "rish_napi",
    nullptr,
    {nullptr},
};

extern "C" __attribute__((constructor)) void RegisterRishModule() {
    napi_module_register(&module);
}
