#include <cctype>
#include <cstdint>
#include <iostream>
#include <stdexcept>
#include <string>
#include <vector>

#include "rish.h"

namespace {

using JsonOperation = char *(*)(const char *, size_t);

std::string Invoke(JsonOperation operation, const std::string &request) {
    char *raw = operation(request.data(), request.size());
    if (raw == nullptr) {
        throw std::runtime_error("Rust JSON operation returned null");
    }
    std::string response(raw);
    rish_string_free(raw);
    return response;
}

std::string JsonString(const std::string &value) {
    std::string output = "\"";
    for (const unsigned char byte : value) {
        switch (byte) {
            case '"':
                output += "\\\"";
                break;
            case '\\':
                output += "\\\\";
                break;
            case '\n':
                output += "\\n";
                break;
            case '\r':
                output += "\\r";
                break;
            case '\t':
                output += "\\t";
                break;
            default:
                if (byte < 0x20) {
                    throw std::runtime_error("unsupported control byte in path");
                }
                output.push_back(static_cast<char>(byte));
        }
    }
    output.push_back('"');
    return output;
}

void RequireContains(
    const std::string &value,
    const std::string &expected,
    const std::string &operation
) {
    if (value.find(expected) == std::string::npos) {
        throw std::runtime_error(
            operation + " response did not contain " + expected + ": " + value
        );
    }
}

std::string ExtractAsciiStdout(const std::string &response) {
    const std::string key = "\"stdout\":";
    size_t cursor = response.find(key);
    if (cursor == std::string::npos) {
        throw std::runtime_error("response has no stdout field: " + response);
    }
    cursor = response.find('[', cursor + key.size());
    if (cursor == std::string::npos) {
        throw std::runtime_error("stdout is not a byte array: " + response);
    }
    cursor++;

    std::string output;
    while (cursor < response.size()) {
        while (cursor < response.size() &&
               (std::isspace(static_cast<unsigned char>(response[cursor])) ||
                response[cursor] == ',')) {
            cursor++;
        }
        if (cursor >= response.size()) {
            break;
        }
        if (response[cursor] == ']') {
            return output;
        }
        size_t end = cursor;
        while (end < response.size() &&
               std::isdigit(static_cast<unsigned char>(response[end]))) {
            end++;
        }
        if (end == cursor) {
            throw std::runtime_error("stdout contains a non-byte value");
        }
        const unsigned long byte =
            std::stoul(response.substr(cursor, end - cursor));
        if (byte > 0x7F) {
            throw std::runtime_error("demo stdout contains a non-ASCII byte");
        }
        output.push_back(static_cast<char>(byte));
        cursor = end;
    }
    throw std::runtime_error("unterminated stdout byte array");
}

std::string ExecuteRequest(
    const std::string &sandbox,
    const std::string &program,
    const std::string &args,
    const std::string &stdin_bytes
) {
    return
        "{\"protocol_version\":1,\"sandbox_root\":" + JsonString(sandbox) +
        ",\"command\":{\"program\":" + JsonString(program) +
        ",\"args\":" + args +
        ",\"env\":{},\"cwd\":\"/\",\"stdin\":" + stdin_bytes + "}}";
}

}  // namespace

int main(int argc, char **argv) {
    try {
        if (argc != 2) {
            throw std::runtime_error(
                "usage: rish-harmony-host-smoke <canonical-sandbox>"
            );
        }
        if (rish_protocol_version() != 1) {
            throw std::runtime_error("unexpected Rust protocol version");
        }

        const std::string grep_request =
            "{\"platform\":\"harmony\",\"privilege\":\"app_sandbox\","
            "\"command\":{\"program\":\"grep\",\"args\":[\"-F\",\"needle\"],"
            "\"env\":{},\"cwd\":\"/\","
            "\"stdin\":[110,101,101,100,108,101,10]}}";
        const std::string grep_plan =
            Invoke(rish_plan_json, grep_request);
        RequireContains(grep_plan, "\"ok\":true", "grep planning");
        RequireContains(
            grep_plan,
            "\"kind\":\"portable_applet\",\"name\":\"grep\"",
            "grep planning"
        );

        const std::string echo_response = Invoke(
            rish_execute_applet_json,
            ExecuteRequest(
                argv[1],
                "echo",
                "[\"hello\",\"from\",\"HarmonyOS\"]",
                "[]"
            )
        );
        RequireContains(echo_response, "\"ok\":true", "echo");
        RequireContains(
            echo_response,
            "\"kind\":\"portable_applet\",\"name\":\"echo\"",
            "echo"
        );
        const std::string echo_stdout = ExtractAsciiStdout(echo_response);
        if (echo_stdout != "hello from HarmonyOS\n") {
            throw std::runtime_error("echo output mismatch");
        }

        const std::string checksum_response = Invoke(
            rish_execute_applet_json,
            ExecuteRequest(
                argv[1],
                "sha256sum",
                "[]",
                "[104,101,108,108,111,32,102,114,111,109,32,72,97,114,109,"
                "111,110,121,79,83,10]"
            )
        );
        RequireContains(checksum_response, "\"ok\":true", "sha256sum");
        RequireContains(
            checksum_response,
            "\"kind\":\"portable_applet\",\"name\":\"sha256sum\"",
            "sha256sum"
        );
        const std::string checksum_stdout =
            ExtractAsciiStdout(checksum_response);
        const std::string expected_checksum =
            "455788b5b41c3bcbb2cd5ef572079294a44a8ed1b7b74c17ec0680cd2bf97f27"
            "  -\n";
        if (checksum_stdout != expected_checksum) {
            throw std::runtime_error("sha256sum output mismatch");
        }

        std::cout
            << "protocol=1\n"
            << "grep_plan=portable_applet:grep\n"
            << "echo=" << echo_stdout
            << "sha256sum=" << checksum_stdout
            << "host_smoke=PASS (macOS C ABI; not a HarmonyOS device run)\n";
        return 0;
    } catch (const std::exception &error) {
        std::cerr << "host_smoke=FAIL: " << error.what() << '\n';
        return 1;
    }
}
