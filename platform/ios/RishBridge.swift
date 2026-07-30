import Foundation

/// Thin Swift adapter around the versioned JSON C ABI.
///
/// Host calls returned by `plan` must be dispatched to explicitly registered
/// Swift/Objective-C handlers. An unknown operation must fail closed.
public enum RishBridge {
    public static func plan(request: Data) throws -> Data {
        guard let request = String(data: request, encoding: .utf8) else {
            throw BridgeError.invalidUTF8
        }

        return try request.withCString { input in
            guard let rawResponse = rish_plan_json(input) else {
                throw BridgeError.nullResponse
            }
            defer { rish_string_free(rawResponse) }
            return Data(String(cString: rawResponse).utf8)
        }
    }

    public enum BridgeError: Error {
        case invalidUTF8
        case nullResponse
    }
}
