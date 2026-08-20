import Darwin
import Foundation

final class RegistryFetcher {
    private static let tokenLimit: UInt64 = 1_048_576
    private let context: PullContext

    init(context: PullContext) {
        self.context = context
    }

    func fetch(_ request: RegistryRequest, bodyFD: Int32) throws -> RegistryHTTPResponse {
        guard !context.isCancelled else { throw RegistryFetchError.cancelled }
        let urlRequest = try Self.makeURLRequest(request)
        try Self.reset(bodyFD)
        context.emit(phase: Self.phase(request), path: request.pathAndQuery, received: 0, expected: nil)

        var response = try perform(
            urlRequest,
            limit: request.maxResponseBytes,
            bodyFD: bodyFD,
            path: request.pathAndQuery,
            countProgress: true
        )
        if response.status == 401,
           let challenge = Self.header("www-authenticate", in: response.headers),
           let bearer = try Self.bearerChallenge(challenge)
        {
            context.emit(
                phase: .authenticating,
                path: request.pathAndQuery,
                received: 0,
                expected: nil
            )
            let token = try fetchToken(bearer)
            try Self.reset(bodyFD)
            var authenticated = urlRequest
            authenticated.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
            response = try perform(
                authenticated,
                limit: request.maxResponseBytes,
                bodyFD: bodyFD,
                path: request.pathAndQuery,
                countProgress: true
            )
        }
        guard !context.isCancelled else { throw RegistryFetchError.cancelled }
        context.emit(phase: .verifying, path: request.pathAndQuery, received: 0, expected: nil)
        return response
    }

    private func fetchToken(_ challenge: BearerChallenge) throws -> String {
        guard var components = URLComponents(url: challenge.realm, resolvingAgainstBaseURL: false),
              components.scheme?.lowercased() == "https",
              components.user == nil,
              components.password == nil,
              components.host?.isEmpty == false
        else {
            throw RegistryFetchError(message: "unsafe bearer token realm", retryable: false)
        }
        var items = components.queryItems ?? []
        if let service = challenge.service {
            items.append(URLQueryItem(name: "service", value: service))
        }
        if let scope = challenge.scope {
            items.append(URLQueryItem(name: "scope", value: scope))
        }
        components.queryItems = items
        guard let url = components.url else {
            throw RegistryFetchError(message: "invalid bearer token realm", retryable: false)
        }
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.setValue("identity", forHTTPHeaderField: "Accept-Encoding")
        let response = try perform(
            request,
            limit: Self.tokenLimit,
            bodyFD: nil,
            path: "",
            countProgress: false
        )
        guard response.status == 200, let body = response.body else {
            throw RegistryFetchError(message: "bearer token request was rejected", retryable: false)
        }
        struct TokenResponse: Decodable {
            let token: String?
            let accessToken: String?

            private enum CodingKeys: String, CodingKey {
                case token
                case accessToken = "access_token"
            }
        }
        let decoded: TokenResponse
        do {
            decoded = try JSONDecoder().decode(TokenResponse.self, from: body)
        } catch {
            throw RegistryFetchError(message: "invalid bearer token response", retryable: false)
        }
        guard let token = decoded.token ?? decoded.accessToken,
              !token.isEmpty,
              token.utf8.count <= 16_384,
              token.utf8.allSatisfy({
                  (48...57).contains($0)
                      || (65...90).contains($0)
                      || (97...122).contains($0)
                      || "-._~+/=".utf8.contains($0)
              })
        else {
            throw RegistryFetchError(message: "invalid bearer token", retryable: false)
        }
        return token
    }

    private func perform(
        _ request: URLRequest,
        limit: UInt64,
        bodyFD: Int32?,
        path: String,
        countProgress: Bool
    ) throws -> RegistryHTTPResponse {
        do {
            return try performOnce(
                request,
                limit: limit,
                bodyFD: bodyFD,
                path: path,
                countProgress: countProgress
            )
        } catch let error as RegistryFetchError
            where error.retryable
                && !context.isCancelled
                && ["GET", "HEAD"].contains(request.httpMethod ?? "")
        {
            if let bodyFD {
                try Self.reset(bodyFD)
            }
            return try performOnce(
                request,
                limit: limit,
                bodyFD: bodyFD,
                path: path,
                countProgress: countProgress
            )
        }
    }

    private func performOnce(
        _ request: URLRequest,
        limit: UInt64,
        bodyFD: Int32?,
        path: String,
        countProgress: Bool
    ) throws -> RegistryHTTPResponse {
        let delegate = RegistrySessionDelegate(
            context: context,
            limit: limit,
            bodyFD: bodyFD,
            path: path,
            countProgress: countProgress
        )
        let configuration = URLSessionConfiguration.ephemeral
        configuration.httpShouldSetCookies = false
        configuration.httpCookieStorage = nil
        configuration.urlCredentialStorage = nil
        configuration.urlCache = nil
        configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
        configuration.timeoutIntervalForRequest = 120
        configuration.timeoutIntervalForResource = 1_200
        let queue = OperationQueue()
        queue.maxConcurrentOperationCount = 1
        let session = URLSession(configuration: configuration, delegate: delegate, delegateQueue: queue)
        guard context.register(session) else {
            session.invalidateAndCancel()
            throw RegistryFetchError.cancelled
        }
        defer {
            context.unregister(session)
            session.finishTasksAndInvalidate()
        }
        session.dataTask(with: request).resume()
        return try delegate.wait()
    }

    private static func makeURLRequest(_ request: RegistryRequest) throws -> URLRequest {
        guard request.scheme.lowercased() == "https",
              ["GET", "HEAD", "POST"].contains(request.method),
              request.pathAndQuery.hasPrefix("/"),
              !request.pathAndQuery.hasPrefix("//"),
              !request.pathAndQuery.contains("\r"),
              !request.pathAndQuery.contains("\n"),
              !request.pathAndQuery.contains("#"),
              request.pathAndQuery.utf8.count <= 16_384,
              let base = URLComponents(string: "https://\(request.authority)"),
              base.host?.isEmpty == false,
              base.user == nil,
              base.password == nil,
              base.path.isEmpty,
              base.query == nil,
              base.fragment == nil,
              let url = URL(string: "https://\(request.authority)\(request.pathAndQuery)"),
              Self.isSafeHTTPS(url)
        else {
            throw RegistryFetchError(message: "unsafe registry request URL", retryable: false)
        }
        var result = URLRequest(url: url)
        result.httpMethod = request.method
        if !request.body.isEmpty {
            guard request.body.count <= 1_048_576 else {
                throw RegistryFetchError(message: "registry request body is too large", retryable: false)
            }
            result.httpBody = Data(request.body)
        }
        var fields = 0
        var bytes = 0
        for (name, values) in request.headers {
            guard !values.isEmpty, Self.safeHeaderName(name) else {
                throw RegistryFetchError(message: "invalid registry request header", retryable: false)
            }
            if ["host", "content-length", "connection", "transfer-encoding",
                "proxy-authorization"].contains(name.lowercased())
            {
                continue
            }
            for value in values {
                fields += 1
                bytes += name.utf8.count + value.utf8.count + 4
                guard fields <= 128,
                      bytes <= 65_536,
                      value.utf8.count <= 16_384,
                      Self.safeHeaderValue(value)
                else {
                    throw RegistryFetchError(message: "invalid registry request header", retryable: false)
                }
                result.addValue(value, forHTTPHeaderField: name)
            }
        }
        result.setValue("identity", forHTTPHeaderField: "Accept-Encoding")
        return result
    }

    private static func reset(_ fd: Int32) throws {
        guard fd >= 0, ftruncate(fd, 0) == 0, lseek(fd, 0, SEEK_SET) >= 0 else {
            throw RegistryFetchError(message: "invalid registry body descriptor", retryable: false)
        }
    }

    private static func phase(_ request: RegistryRequest) -> RishImagePullPhase {
        request.pathAndQuery.contains("/manifests/") ? .downloadingManifest : .downloadingBlob
    }

    private static func header(_ name: String, in headers: [String: [String]]) -> String? {
        headers[name.lowercased()]?.first
    }

    static func isSafeHTTPS(_ url: URL) -> Bool {
        url.scheme?.lowercased() == "https"
            && url.user == nil
            && url.password == nil
            && url.host?.isEmpty == false
    }

    static func safeHeaderName(_ name: String) -> Bool {
        !name.isEmpty && name.utf8.count <= 128 && name.utf8.allSatisfy {
            (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0)
                || "!#$%&'*+-.^_`|~".utf8.contains($0)
        }
    }

    static func safeHeaderValue(_ value: String) -> Bool {
        value.utf8.allSatisfy { $0 == 9 || (32...126).contains($0) }
    }

    private struct BearerChallenge {
        let realm: URL
        let service: String?
        let scope: String?
    }

    private static func bearerChallenge(_ header: String) throws -> BearerChallenge? {
        let trimmed = header.trimmingCharacters(in: .whitespaces)
        guard trimmed.prefix(7).caseInsensitiveCompare("Bearer ") == .orderedSame else {
            return nil
        }
        var fields: [String: String] = [:]
        var input = trimmed.dropFirst(7)[...]
        while !input.isEmpty {
            while input.first == " " || input.first == "," { input.removeFirst() }
            guard !input.isEmpty else { break }
            guard let equals = input.firstIndex(of: "=") else {
                throw RegistryFetchError(message: "invalid bearer challenge", retryable: false)
            }
            let key = input[..<equals].trimmingCharacters(in: .whitespaces).lowercased()
            input = input[input.index(after: equals)...]
            guard input.first == "\"" else {
                throw RegistryFetchError(message: "invalid bearer challenge", retryable: false)
            }
            input.removeFirst()
            var value = ""
            var escaped = false
            var closed = false
            while let character = input.first {
                input.removeFirst()
                if escaped {
                    value.append(character)
                    escaped = false
                } else if character == "\\" {
                    escaped = true
                } else if character == "\"" {
                    closed = true
                    break
                } else {
                    value.append(character)
                }
            }
            guard closed, !key.isEmpty, fields.updateValue(value, forKey: key) == nil else {
                throw RegistryFetchError(message: "invalid bearer challenge", retryable: false)
            }
        }
        guard let realmValue = fields["realm"],
              let realm = URL(string: realmValue),
              isSafeHTTPS(realm)
        else {
            throw RegistryFetchError(message: "invalid bearer challenge realm", retryable: false)
        }
        return BearerChallenge(realm: realm, service: fields["service"], scope: fields["scope"])
    }
}

private final class RegistrySessionDelegate:
    NSObject,
    URLSessionDataDelegate,
    URLSessionTaskDelegate,
    @unchecked Sendable
{
    private let lock = NSLock()
    private let semaphore = DispatchSemaphore(value: 0)
    private let context: PullContext
    private let limit: UInt64
    private let bodyFD: Int32?
    private let path: String
    private let countProgress: Bool
    private var response: HTTPURLResponse?
    private var body = Data()
    private var received: UInt64 = 0
    private var expected: UInt64?
    private var redirectCount = 0
    private var result: Result<RegistryHTTPResponse, RegistryFetchError>?

    init(
        context: PullContext,
        limit: UInt64,
        bodyFD: Int32?,
        path: String,
        countProgress: Bool
    ) {
        self.context = context
        self.limit = limit
        self.bodyFD = bodyFD
        self.path = path
        self.countProgress = countProgress
    }

    func wait() throws -> RegistryHTTPResponse {
        semaphore.wait()
        lock.lock()
        let result = result
        lock.unlock()
        guard let result else {
            throw RegistryFetchError(message: "registry request ended without a result", retryable: false)
        }
        return try result.get()
    }

    func urlSession(
        _ session: URLSession,
        dataTask: URLSessionDataTask,
        didReceive response: URLResponse,
        completionHandler: @escaping (URLSession.ResponseDisposition) -> Void
    ) {
        guard let response = response as? HTTPURLResponse else {
            fail("registry returned a non-HTTP response", retryable: false, task: dataTask)
            completionHandler(.cancel)
            return
        }
        let declared: UInt64?
        if let value = response.value(forHTTPHeaderField: "Content-Length") {
            guard !value.isEmpty,
                  value.utf8.allSatisfy({ (48...57).contains($0) }),
                  let length = UInt64(value)
            else {
                fail("registry returned an invalid Content-Length", retryable: false, task: dataTask)
                completionHandler(.cancel)
                return
            }
            declared = length
        } else if response.expectedContentLength >= 0 {
            declared = UInt64(response.expectedContentLength)
        } else {
            declared = nil
        }
        if let declared, declared > limit {
            fail("registry response exceeds its declared limit", retryable: false, task: dataTask)
            completionHandler(.cancel)
            return
        }
        if let encoding = response.value(forHTTPHeaderField: "Content-Encoding"),
           encoding.caseInsensitiveCompare("identity") != .orderedSame
        {
            fail("registry ignored identity content encoding", retryable: false, task: dataTask)
            completionHandler(.cancel)
            return
        }
        self.response = response
        expected = declared
        if countProgress, (200...299).contains(response.statusCode) {
            context.emit(
                phase: path.contains("/manifests/") ? .downloadingManifest : .downloadingBlob,
                path: path,
                received: 0,
                expected: declared
            )
        }
        completionHandler(.allow)
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
        guard result == nil else { return }
        let (next, overflow) = received.addingReportingOverflow(UInt64(data.count))
        guard !overflow, next <= limit else {
            fail("registry response exceeded its streaming limit", retryable: false, task: dataTask)
            return
        }
        received = next
        if let response, (200...299).contains(response.statusCode) {
            if let bodyFD {
                do {
                    try Self.write(data, to: bodyFD)
                } catch {
                    fail("failed to stream registry response", retryable: false, task: dataTask)
                    return
                }
            } else {
                body.append(data)
            }
            if countProgress {
                context.emit(
                    phase: path.contains("/manifests/") ? .downloadingManifest : .downloadingBlob,
                    path: path,
                    received: next,
                    expected: expected,
                    aggregateDelta: UInt64(data.count)
                )
            }
        }
    }

    func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        willPerformHTTPRedirection response: HTTPURLResponse,
        newRequest request: URLRequest,
        completionHandler: @escaping (URLRequest?) -> Void
    ) {
        redirectCount += 1
        guard redirectCount <= 5,
              let source = response.url,
              let target = request.url,
              RegistryFetcher.isSafeHTTPS(target)
        else {
            fail("unsafe or excessive registry redirect", retryable: false, task: task)
            completionHandler(nil)
            return
        }
        var redirected = request
        if Self.origin(source) != Self.origin(target) {
            redirected.setValue(nil, forHTTPHeaderField: "Authorization")
            redirected.setValue(nil, forHTTPHeaderField: "Proxy-Authorization")
        }
        redirected.setValue("identity", forHTTPHeaderField: "Accept-Encoding")
        completionHandler(redirected)
    }

    func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        didCompleteWithError error: Error?
    ) {
        lock.lock()
        guard result == nil else {
            lock.unlock()
            return
        }
        if let error {
            let code = (error as? URLError)?.code
            let cancelled = code == .cancelled || context.isCancelled
            let message = code.map {
                "registry network request failed (URL error \($0.rawValue))"
            } ?? "registry network request failed"
            result = .failure(
                cancelled
                    ? .cancelled
                    : RegistryFetchError(
                        message: message,
                        retryable: Self.retryable(code)
                    )
            )
        } else if let response {
            do {
                let headers = try Self.headers(response)
                result = .success(
                    RegistryHTTPResponse(
                        status: response.statusCode,
                        headers: headers,
                        body: bodyFD == nil ? body : nil
                    )
                )
            } catch let error as RegistryFetchError {
                result = .failure(error)
            } catch {
                result = .failure(
                    RegistryFetchError(message: "invalid registry response headers", retryable: false)
                )
            }
        } else {
            result = .failure(
                RegistryFetchError(message: "registry response was missing", retryable: false)
            )
        }
        lock.unlock()
        semaphore.signal()
    }

    private func fail(
        _ message: String,
        retryable: Bool,
        task: URLSessionTask
    ) {
        lock.lock()
        guard result == nil else {
            lock.unlock()
            return
        }
        result = .failure(RegistryFetchError(message: message, retryable: retryable))
        lock.unlock()
        task.cancel()
        semaphore.signal()
    }

    private static func write(_ data: Data, to fd: Int32) throws {
        try data.withUnsafeBytes { bytes in
            guard let base = bytes.baseAddress else { return }
            var offset = 0
            while offset < bytes.count {
                let written = Darwin.write(fd, base.advanced(by: offset), bytes.count - offset)
                if written > 0 {
                    offset += written
                } else if written < 0, errno == EINTR {
                    continue
                } else {
                    throw RegistryFetchError(
                        message: "registry body descriptor write failed",
                        retryable: false
                    )
                }
            }
        }
    }

    private static func headers(_ response: HTTPURLResponse) throws -> [String: [String]] {
        var result: [String: [String]] = [:]
        var fields = 0
        var bytes = 0
        for (rawName, rawValue) in response.allHeaderFields {
            let name = String(describing: rawName).lowercased()
            let value = String(describing: rawValue)
            fields += 1
            bytes += name.utf8.count + value.utf8.count + 4
            guard fields <= 128,
                  bytes <= 65_536,
                  RegistryFetcher.safeHeaderName(name),
                  value.utf8.count <= 16_384,
                  RegistryFetcher.safeHeaderValue(value)
            else {
                throw RegistryFetchError(message: "invalid registry response headers", retryable: false)
            }
            result[name, default: []].append(value)
        }
        return result
    }

    private static func origin(_ url: URL) -> String {
        let port = url.port ?? 443
        return "\(url.scheme?.lowercased() ?? "")://\(url.host?.lowercased() ?? ""):\(port)"
    }

    private static func retryable(_ code: URLError.Code?) -> Bool {
        guard let code else { return false }
        return [
            .timedOut,
            .cannotFindHost,
            .cannotConnectToHost,
            .networkConnectionLost,
            .dnsLookupFailed,
            .notConnectedToInternet,
            .secureConnectionFailed,
            .internationalRoamingOff,
            .callIsActive,
            .dataNotAllowed
        ].contains(code)
    }
}
