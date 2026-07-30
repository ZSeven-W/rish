#include <cstring>
#include <string>

#include "napi/native_api.h"
#include "napi/native_node_api.h"

#include "../rish.h"

namespace {

napi_value PlanJson(napi_env env, napi_callback_info info) {
    size_t argc = 1;
    napi_value argv[1] = {nullptr};
    napi_get_cb_info(env, info, &argc, argv, nullptr, nullptr);

    if (argc != 1) {
        napi_throw_type_error(env, nullptr, "planJson expects one JSON string");
        return nullptr;
    }

    size_t request_size = 0;
    if (napi_get_value_string_utf8(env, argv[0], nullptr, 0, &request_size) != napi_ok) {
        napi_throw_type_error(env, nullptr, "request must be a UTF-8 string");
        return nullptr;
    }

    std::string request(request_size, '\0');
    napi_get_value_string_utf8(
        env,
        argv[0],
        request.data(),
        request.size() + 1,
        &request_size
    );

    char *response = rish_plan_json(request.c_str());
    if (response == nullptr) {
        napi_throw_error(env, nullptr, "Rust planner returned null");
        return nullptr;
    }

    napi_value result = nullptr;
    napi_create_string_utf8(env, response, std::strlen(response), &result);
    rish_string_free(response);
    return result;
}

napi_value Init(napi_env env, napi_value exports) {
    napi_property_descriptor properties[] = {
        {"planJson", nullptr, PlanJson, nullptr, nullptr, nullptr, napi_default, nullptr},
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
    .nm_version = 1,
    .nm_flags = 0,
    .nm_filename = nullptr,
    .nm_register_func = Init,
    .nm_modname = "rish_ffi",
    .nm_priv = nullptr,
    .reserved = {nullptr},
};

extern "C" __attribute__((constructor)) void RegisterRishModule() {
    napi_module_register(&module);
}
