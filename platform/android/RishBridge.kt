package dev.rish.runtime

import android.content.Context
import java.io.File
import java.util.concurrent.CancellationException
import java.util.concurrent.atomic.AtomicBoolean
import org.json.JSONArray
import org.json.JSONObject

/** JSON-compatible counterpart of `rish_core::GuestCommand`. */
data class RishGuestCommand(
    val program: String,
    val args: List<String> = emptyList(),
    val env: Map<String, String> = emptyMap(),
    val cwd: String = "/",
    /** Encoded as a JSON integer array, matching Rust `Vec<u8>`. */
    val stdin: ByteArray = byteArrayOf(),
)

data class RishAppletLimits(
    val maxInputBytes: Int = 1_048_576,
    val maxOutputBytes: Int = 1_048_576,
    val maxFilesystemEntries: Int = 10_000,
    val maxRecursionDepth: Int = 64,
) {
    init {
        require(maxInputBytes in 1..1_048_576)
        require(maxOutputBytes in 1..1_048_576)
        require(maxFilesystemEntries in 1..100_000)
        require(maxRecursionDepth in 1..256)
    }
}

/**
 * Host-owned sandbox configuration. Paths must come from Android Context or
 * other application code, never a guest command or planner payload.
 */
class RishAppletConfiguration private constructor(
    internal val sandboxRoot: String,
    internal val readOnly: Boolean,
    internal val user: String,
    internal val hostname: String,
    internal val limits: RishAppletLimits,
) {
    companion object {
        fun inAppFiles(
            context: Context,
            directoryName: String = "rish-applets",
            readOnly: Boolean = false,
            user: String = "rish",
            hostname: String = "rish",
            limits: RishAppletLimits = RishAppletLimits(),
        ): RishAppletConfiguration {
            require(directoryName.matches(Regex("[A-Za-z0-9._-]{1,64}")))
            return withinAppContainer(
                context.filesDir,
                File(context.filesDir, directoryName),
                readOnly,
                user,
                hostname,
                limits,
            )
        }

        private fun withinAppContainer(
            appContainerRoot: File,
            sandboxRoot: File,
            readOnly: Boolean = false,
            user: String = "rish",
            hostname: String = "rish",
            limits: RishAppletLimits = RishAppletLimits(),
        ): RishAppletConfiguration {
            require(appContainerRoot.isAbsolute && sandboxRoot.isAbsolute)
            require(safeIdentity(user) && safeIdentity(hostname))
            val container = appContainerRoot.canonicalFile
            require(container.isDirectory)
            val sandbox = sandboxRoot.canonicalFile
            require(isStrictDescendant(sandbox, container))
            require(sandbox.mkdirs() || sandbox.isDirectory)
            val verified = sandbox.canonicalFile
            require(verified.isDirectory && isStrictDescendant(verified, container))
            return RishAppletConfiguration(
                verified.path,
                readOnly,
                user,
                hostname,
                limits,
            )
        }

        private fun isStrictDescendant(child: File, parent: File): Boolean =
            child.path.startsWith(parent.path.trimEnd(File.separatorChar) + File.separator)

        private fun safeIdentity(value: String): Boolean =
            value.matches(Regex("[A-Za-z0-9._-]{1,64}"))
    }
}

/** JSON-compatible counterpart of `rish_core::CapabilityRequirement`. */
data class RishCapabilityRequirement(
    val capability: String,
    val kernelSemanticsRequired: Boolean = false,
)

/** JSON-compatible counterpart of `rish_core::HostCall`. */
data class RishHostCall(
    val protocolVersion: Int,
    val id: Long,
    val operation: String,
    val command: RishGuestCommand,
    val requirements: List<RishCapabilityRequirement> = emptyList(),
    /** A JSONObject, JSONArray, JSON primitive, or null. */
    val payload: Any? = null,
)

/** JSON-compatible counterpart of `rish_core::HostReply`. */
data class RishHostReply(
    val exitCode: Int,
    /** These remain binary and are serialized as integer arrays, never text. */
    val stdout: ByteArray = byteArrayOf(),
    val stderr: ByteArray = byteArrayOf(),
    val payload: Any? = null,
) {
    companion object {
        fun failure(code: String, message: String, exitCode: Int = 125): RishHostReply {
            val error = JSONObject()
                .put("code", code)
                .put("message", message)
            return RishHostReply(
                exitCode = exitCode,
                stderr = "$message\n".encodeToByteArray(),
                payload = JSONObject().put("error", error),
            )
        }
    }
}

/**
 * Strict codec for the Rust HostCall/HostReply JSON wire format.
 *
 * Android's JSONObject is used to avoid imposing a serialization dependency on
 * embedders. Rust `u64` call ids above `Long.MAX_VALUE` fail closed.
 */
object RishHostCodec {
    fun decodeHostCall(json: String): RishHostCall {
        require(json.length <= MAX_WIRE_CHARACTERS) {
            "host call exceeds the JSON wire size limit"
        }
        val root = JSONObject(json)
        val protocolVersion = root.requiredInt("protocol_version")
        val id = root.requiredLong("id")
        require(id >= 0) { "id must fit a non-negative signed 64-bit integer" }
        val operation = root.requiredString("operation")
        require(operation.isNotBlank()) { "operation must not be blank" }

        val commandObject = root.getJSONObject("command")
        val command = RishGuestCommand(
            program = commandObject.requiredString("program"),
            args = commandObject.optionalStringList("args"),
            env = commandObject.optionalStringMap("env"),
            cwd =
                if (commandObject.has("cwd")) commandObject.requiredString("cwd") else "/",
            stdin = commandObject.optionalByteArray("stdin"),
        )

        val requirements = if (root.has("requirements")) {
            val array = root.getJSONArray("requirements")
            List(array.length()) { index ->
                val item = array.getJSONObject(index)
                RishCapabilityRequirement(
                    capability = item.requiredString("capability"),
                    kernelSemanticsRequired =
                        item.optionalBoolean("kernel_semantics_required"),
                )
            }
        } else {
            emptyList()
        }

        return RishHostCall(
            protocolVersion = protocolVersion,
            id = id,
            operation = operation,
            command = command,
            requirements = requirements,
            payload = root.jsonValueOrNull("payload"),
        )
    }

    fun encodeHostReply(reply: RishHostReply): String {
        return JSONObject()
            .put("exit_code", reply.exitCode)
            .put("stdout", reply.stdout.toJsonArray())
            .put("stderr", reply.stderr.toJsonArray())
            .put("payload", reply.payload ?: JSONObject.NULL)
            .toString()
    }

    internal fun encodeGuestCommand(command: RishGuestCommand): JSONObject {
        val args = JSONArray()
        command.args.forEach { args.put(it) }
        val environment = JSONObject()
        command.env.forEach { (key, value) -> environment.put(key, value) }
        return JSONObject()
            .put("program", command.program)
            .put("args", args)
            .put("env", environment)
            .put("cwd", command.cwd)
            .put("stdin", command.stdin.toJsonArray())
    }

    internal fun encodeAppletLimits(limits: RishAppletLimits): JSONObject =
        JSONObject()
            .put("max_input_bytes", limits.maxInputBytes)
            .put("max_output_bytes", limits.maxOutputBytes)
            .put("max_filesystem_entries", limits.maxFilesystemEntries)
            .put("max_recursion_depth", limits.maxRecursionDepth)

    private fun JSONObject.requiredString(key: String): String {
        val value = getString(key)
        require(value.isNotEmpty()) { "$key must not be empty" }
        return value
    }

    private fun JSONObject.requiredInt(key: String): Int {
        val value = requiredLong(key)
        require(value in Int.MIN_VALUE..Int.MAX_VALUE) { "$key is outside Int range" }
        return value.toInt()
    }

    private fun JSONObject.requiredLong(key: String): Long {
        val value = get(key)
        require(value is Number) { "$key must be an integer" }
        return value.toString().toLongOrNull()
            ?: throw IllegalArgumentException("$key must be an exact signed 64-bit integer")
    }

    private fun JSONObject.optionalStringList(key: String): List<String> {
        if (!has(key)) {
            return emptyList()
        }
        val array = getJSONArray(key)
        return List(array.length()) { index -> array.getString(index) }
    }

    private fun JSONObject.optionalBoolean(key: String): Boolean {
        if (!has(key)) {
            return false
        }
        val value = get(key)
        require(value is Boolean) { "$key must be a boolean" }
        return value
    }

    private fun JSONObject.optionalStringMap(key: String): Map<String, String> {
        if (!has(key)) {
            return emptyMap()
        }
        val objectValue = getJSONObject(key)
        val result = linkedMapOf<String, String>()
        val keys = objectValue.keys()
        while (keys.hasNext()) {
            val item = keys.next()
            result[item] = objectValue.getString(item)
        }
        return result
    }

    private fun JSONObject.optionalByteArray(key: String): ByteArray {
        if (!has(key)) {
            return byteArrayOf()
        }
        val array = getJSONArray(key)
        return ByteArray(array.length()) { index ->
            val encoded = array.get(index)
            require(encoded is Number) { "$key[$index] is not a number" }
            val value = encoded.toString().toIntOrNull()
                ?: throw IllegalArgumentException("$key[$index] is not an integer")
            require(value in 0..255) { "$key[$index] is not a byte" }
            value.toByte()
        }
    }

    private fun JSONObject.jsonValueOrNull(key: String): Any? {
        if (!has(key) || isNull(key)) {
            return null
        }
        return get(key)
    }

    private fun ByteArray.toJsonArray(): JSONArray {
        val array = JSONArray()
        forEach { byte -> array.put(byte.toInt() and 0xff) }
        return array
    }

    private const val MAX_WIRE_CHARACTERS: Int = 8 * 1024 * 1024
}

/**
 * Cooperative cancellation independent of a particular coroutine library.
 *
 * The coroutine owner should call [cancel] when its lifecycle/job is
 * cancelled. Injected long-running handlers must check this token.
 */
class RishCancellationToken {
    private val cancelled = AtomicBoolean(false)

    val isCancelled: Boolean
        get() = cancelled.get()

    fun cancel() {
        cancelled.set(true)
    }

    fun throwIfCancelled() {
        if (isCancelled) {
            throw CancellationException("rish host operation cancelled")
        }
    }
}

/** Asynchronous native handler with binary-safe output and cancellation. */
interface RishHostOperationHandler {
    suspend fun handle(
        call: RishHostCall,
        cancellation: RishCancellationToken,
    ): RishHostReply
}

/**
 * App-local service state only. This is not systemd, PID 1, or a cgroup
 * manager, and it never launches a Linux service process.
 */
class RishInMemoryServiceSupervisor : RishHostOperationHandler {
    private enum class State(val wireName: String) {
        ACTIVE("active"),
        INACTIVE("inactive"),
    }

    private val lock = Any()
    private val units = linkedMapOf<String, State>()

    override suspend fun handle(
        call: RishHostCall,
        cancellation: RishCancellationToken,
    ): RishHostReply {
        cancellation.throwIfCancelled()
        if (call.operation != "service.systemctl" ||
            call.command.program.substringAfterLast('/') != "systemctl"
        ) {
            return RishHostReply.failure(
                code = "command_operation_mismatch",
                message = "service.systemctl only accepts the systemctl command",
            )
        }

        val verb = call.command.args.firstOrNull() ?: return usage()
        return synchronized(lock) {
            when (verb) {
                "list-units" -> {
                    if (call.command.args.size != 1) {
                        usage()
                    } else {
                        val output = units.entries
                            .sortedBy { it.key }
                            .joinToString(separator = "\n") {
                                "${it.key} ${it.value.wireName}"
                            }
                            .let { if (it.isEmpty()) it else "$it\n" }
                        reply(0, output, verb, null)
                    }
                }

                "start", "stop", "restart", "status", "is-active" -> {
                    val unit = call.command.args.getOrNull(1)
                    if (call.command.args.size != 2 || unit == null || !isSafeUnitName(unit)) {
                        usage()
                    } else {
                        apply(verb, unit)
                    }
                }

                else -> RishHostReply.failure(
                    code = "unsupported_systemctl_verb",
                    message = "in-memory supervisor does not implement that systemctl verb",
                    exitCode = 2,
                )
            }
        }
    }

    private fun apply(verb: String, unit: String): RishHostReply {
        return when (verb) {
            "start" -> {
                if (!units.containsKey(unit) && units.size >= MAXIMUM_UNITS) {
                    return capacityExceeded()
                }
                units[unit] = State.ACTIVE
                reply(0, "", verb, unit)
            }

            "stop" -> {
                if (units.containsKey(unit)) {
                    units[unit] = State.INACTIVE
                }
                reply(0, "", verb, unit)
            }

            "restart" -> {
                if (!units.containsKey(unit) && units.size >= MAXIMUM_UNITS) {
                    return capacityExceeded()
                }
                units[unit] = State.ACTIVE
                reply(0, "", verb, unit)
            }

            "status" -> {
                val state = units[unit] ?: State.INACTIVE
                reply(
                    if (state == State.ACTIVE) 0 else 3,
                    "$unit: ${state.wireName} (rish in-memory supervisor)\n",
                    verb,
                    unit,
                )
            }

            "is-active" -> {
                val state = units[unit] ?: State.INACTIVE
                reply(
                    if (state == State.ACTIVE) 0 else 3,
                    "${state.wireName}\n",
                    verb,
                    unit,
                )
            }

            else -> RishHostReply.failure(
                code = "internal_dispatch_error",
                message = "invalid service verb",
            )
        }
    }

    private fun reply(
        exitCode: Int,
        stdout: String,
        verb: String,
        unit: String?,
    ): RishHostReply {
        val payload = JSONObject()
            .put("implementation", "rish.in_memory_supervisor")
            .put("real_systemd", false)
            .put("verb", verb)
        if (unit != null) {
            payload.put("unit", unit)
        }
        return RishHostReply(
            exitCode = exitCode,
            stdout = stdout.encodeToByteArray(),
            payload = payload,
        )
    }

    private fun usage(): RishHostReply {
        return RishHostReply.failure(
            code = "invalid_systemctl_arguments",
            message =
                "usage: systemctl <start|stop|restart|status|is-active> <unit> | list-units",
            exitCode = 2,
        )
    }

    private fun capacityExceeded(): RishHostReply {
        return RishHostReply.failure(
            code = "supervisor_capacity_exceeded",
            message = "in-memory supervisor unit limit reached",
        )
    }

    private fun isSafeUnitName(unit: String): Boolean {
        if (unit.isEmpty() || unit.toByteArray().size > 128) {
            return false
        }
        return unit.all { character ->
            character in '0'..'9' ||
                character in 'A'..'Z' ||
                character in 'a'..'z' ||
                character == '-' ||
                character == '.' ||
                character == '@' ||
                character == '_'
        }
    }

    companion object {
        private const val MAXIMUM_UNITS: Int = 256
    }
}

/**
 * Explicit allow-list dispatcher. Only the Docker API facade is injectable;
 * callers cannot register handlers for arbitrary operation names.
 */
class RishHostDispatcher(
    private val supervisor: RishInMemoryServiceSupervisor =
        RishInMemoryServiceSupervisor(),
    private val dockerApiHandler: RishHostOperationHandler? = null,
) {
    suspend fun dispatch(
        call: RishHostCall,
        cancellation: RishCancellationToken = RishCancellationToken(),
    ): RishHostReply {
        if (call.protocolVersion != PROTOCOL_VERSION) {
            return RishHostReply.failure(
                code = "unsupported_protocol_version",
                message = "host supports protocol version $PROTOCOL_VERSION",
            )
        }
        if (!isWithinInputLimits(call)) {
            return RishHostReply.failure(
                code = "host_call_limits_exceeded",
                message = "host call exceeds portable dispatcher limits",
            )
        }
        if (call.requirements.any { it.kernelSemanticsRequired }) {
            return RishHostReply.failure(
                code = "kernel_semantics_unavailable",
                message = "portable Android offload cannot satisfy real Linux kernel semantics",
            )
        }

        return try {
            cancellation.throwIfCancelled()
            val reply = when (call.operation) {
                "service.systemctl" -> supervisor.handle(call, cancellation)
                "container.docker_api" -> {
                    if (call.command.program.substringAfterLast('/') != "docker") {
                        RishHostReply.failure(
                            code = "command_operation_mismatch",
                            message = "container.docker_api only accepts the docker command",
                        )
                    } else {
                        dockerApiHandler?.handle(call, cancellation)
                            ?: RishHostReply.failure(
                                code = "docker_api_unavailable",
                                message =
                                    "Docker API handler is not configured; no dockerd was started",
                            )
                    }
                }

                else -> RishHostReply.failure(
                    code = "operation_not_allow_listed",
                    message = "native operation is not allow-listed",
                    exitCode = 126,
                )
            }
            cancellation.throwIfCancelled()
            if (reply.stdout.size > MAXIMUM_OUTPUT_BYTES ||
                reply.stderr.size > MAXIMUM_OUTPUT_BYTES
            ) {
                RishHostReply.failure(
                    code = "host_reply_limits_exceeded",
                    message = "native handler output exceeds dispatcher limits",
                )
            } else {
                reply
            }
        } catch (_: CancellationException) {
            RishHostReply.failure(
                code = "cancelled",
                message = "native operation was cancelled",
                exitCode = 130,
            )
        } catch (error: Exception) {
            RishHostReply.failure(
                code = "handler_failed",
                message = "native handler failed: ${error.message ?: error.javaClass.simpleName}",
            )
        }
    }

    companion object {
        const val PROTOCOL_VERSION: Int = 1
        private const val MAXIMUM_OUTPUT_BYTES: Int = 8 * 1024 * 1024

        private fun isWithinInputLimits(call: RishHostCall): Boolean {
            return call.operation.toByteArray().size <= 128 &&
                call.command.program.toByteArray().size <= 4_096 &&
                call.command.args.size <= 64 &&
                call.command.args.all { it.toByteArray().size <= 4_096 } &&
                call.command.env.size <= 128 &&
                call.command.env.all {
                    it.key.toByteArray().size <= 1_024 &&
                        it.value.toByteArray().size <= 65_536
                } &&
                call.command.stdin.size <= 1_048_576 &&
                call.requirements.size <= 64 &&
                call.requirements.all {
                    it.capability.toByteArray().size <= 128
                }
        }
    }
}

/**
 * JNI-facing wrapper for the Rust JSON planning ABI plus native dispatch
 * codecs. Planning does not invoke a host handler by itself.
 */
object RishBridge {
    init {
        System.loadLibrary("rish_jni")
    }

    external fun planJson(request: String): String
    private external fun executeAppletJson(request: String): String
    external fun protocolVersion(): Int

    /**
     * Boots the in-repository pure-Rust x86_64 interpreter with an app-supplied
     * kernel and initramfs and runs one command inside the Linux guest — the
     * full docker surface. Call from a background thread: it boots a Linux guest
     * and blocks. The request JSON carries kernel_path, initrd_path, an optional
     * root_disk_path, memory_mib, a command argv array, an optional
     * command_line, and optional boot/handshake budgets. Reply JSON carries ok,
     * exit_code, stdout, stderr, and boot_units, or ok=false with an error.
     */
    external fun vmRunDockerJson(request: String): String

    /**
     * Builds its own Android/app-sandbox plan and invokes Rust only when the
     * returned plan is exactly the matching portable applet.
     */
    fun executePortableApplet(
        command: RishGuestCommand,
        configuration: RishAppletConfiguration,
    ): String {
        require(command.stdin.size <= configuration.limits.maxInputBytes)
        val planRequest = JSONObject()
            .put("platform", "android")
            .put("privilege", "app_sandbox")
            .put("command", RishHostCodec.encodeGuestCommand(command))
            .toString()
        val planned = JSONObject(planJson(planRequest))
        require(planned.getInt("protocol_version") == protocolVersion())
        require(planned.get("ok") == true) {
            planned.optString("error", "planner rejected command")
        }
        val plan = planned.getJSONObject("plan")
        require(plan.getString("kind") == "portable_applet") {
            "plan is not a portable applet"
        }
        require(
            plan.getString("name") == command.program.substringAfterLast('/'),
        ) {
            "portable applet plan does not match command"
        }

        val executeRequest = JSONObject()
            .put("protocol_version", protocolVersion())
            .put("sandbox_root", configuration.sandboxRoot)
            .put("read_only", configuration.readOnly)
            .put("user", configuration.user)
            .put("hostname", configuration.hostname)
            .put("limits", RishHostCodec.encodeAppletLimits(configuration.limits))
            .put("command", RishHostCodec.encodeGuestCommand(command))
            .toString()
        require(executeRequest.length <= 8 * 1024 * 1024)
        return executeAppletJson(executeRequest).also {
            require(it.length <= 8 * 1024 * 1024) {
                "portable applet response exceeds platform bridge limit"
            }
        }
    }

    fun decodeHostCall(json: String): RishHostCall =
        RishHostCodec.decodeHostCall(json)

    fun encodeHostReply(reply: RishHostReply): String =
        RishHostCodec.encodeHostReply(reply)

    suspend fun dispatchHostCall(
        json: String,
        dispatcher: RishHostDispatcher,
        cancellation: RishCancellationToken = RishCancellationToken(),
    ): String {
        val call = decodeHostCall(json)
        return encodeHostReply(dispatcher.dispatch(call, cancellation))
    }
}
